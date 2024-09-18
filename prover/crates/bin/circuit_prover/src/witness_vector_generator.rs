use std::{collections::HashMap, sync::Arc, time::Instant};

use anyhow::Context;
use tokio::{sync::mpsc::Sender, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use zksync_object_store::ObjectStore;
use zksync_prover_dal::{ConnectionPool, Prover, ProverDal};
use zksync_prover_fri_types::{
    circuit_definitions::{
        boojum::{
            cs::implementations::setup::FinalizationHintsForProver,
            field::goldilocks::GoldilocksField,
            gadgets::queue::full_state_queue::FullStateCircuitQueueRawWitness,
        },
        circuit_definitions::base_layer::ZkSyncBaseLayerCircuit,
    },
    get_current_pod_name,
    keys::RamPermutationQueueWitnessKey,
    CircuitAuxData, CircuitWrapper, ProverJob, ProverServiceDataKey, RamPermutationQueueWitness,
    WitnessVectorArtifactsTemp,
};
use zksync_types::{protocol_version::ProtocolSemanticVersion, L1BatchNumber};
use zksync_utils::panic_extractor::try_extract_panic_message;

use crate::{Backoff, WITNESS_VECTOR_GENERATOR_METRICS};

pub struct WitnessVectorGenerator {
    object_store: Arc<dyn ObjectStore>,
    connection_pool: ConnectionPool<Prover>,
    protocol_version: ProtocolSemanticVersion,
    finalization_hints: HashMap<ProverServiceDataKey, Arc<FinalizationHintsForProver>>,
    sender: Sender<WitnessVectorArtifactsTemp>,
    pod_name: String,
}

impl WitnessVectorGenerator {
    pub fn new(
        object_store: Arc<dyn ObjectStore>,
        connection_pool: ConnectionPool<Prover>,
        protocol_version: ProtocolSemanticVersion,
        finalization_hints: HashMap<ProverServiceDataKey, Arc<FinalizationHintsForProver>>,
        sender: Sender<WitnessVectorArtifactsTemp>,
    ) -> Self {
        Self {
            object_store,
            connection_pool,
            protocol_version,
            finalization_hints,
            sender,
            pod_name: get_current_pod_name(),
        }
    }

    pub async fn run(
        self,
        cancellation_token: CancellationToken,
        mut backoff: Backoff,
    ) -> anyhow::Result<()> {
        let mut get_job_timer = Instant::now();
        while !cancellation_token.is_cancelled() {
            if let Some(prover_job) = self
                .get_job()
                .await
                .context("failed to get next witness generation job")?
            {
                tracing::info!(
                    "Witness Vector Generator received job {:?} after: {:?}",
                    prover_job.job_id,
                    get_job_timer.elapsed()
                );
                WITNESS_VECTOR_GENERATOR_METRICS
                    .job_wait_time
                    .observe(get_job_timer.elapsed());
                match self.generate(prover_job, cancellation_token.clone()).await {
                    e @ Err(_) => {
                        if cancellation_token.is_cancelled() {
                            return Ok(());
                        }
                        e.context("failed to generate witness")?
                    }
                    _ => {}
                }

                get_job_timer = Instant::now();
                backoff.reset();
                continue;
            };
            self.backoff(&mut backoff, cancellation_token.clone()).await;
        }
        tracing::info!("Witness Vector Generator shut down.");
        Ok(())
    }

    async fn get_job(&self) -> anyhow::Result<Option<ProverJob>> {
        let mut connection = self
            .connection_pool
            .connection()
            .await
            .context("failed to get connection")?;
        let prover_job_metadata = match connection
            .fri_prover_jobs_dal()
            .get_job(self.protocol_version, &self.pod_name)
            .await
        {
            None => return Ok(None),
            Some(job) => job,
        };

        let time = Instant::now();
        let circuit_wrapper = self
            .object_store
            .get(prover_job_metadata.into())
            .await
            .context("failed to get circuit_wrapper from object store")?;
        let artifact = match circuit_wrapper {
            a @ CircuitWrapper::Base(_) => a,
            a @ CircuitWrapper::Recursive(_) => a,
            CircuitWrapper::BasePartial((circuit, aux_data)) => self
                .fill_witness(circuit, aux_data, prover_job_metadata.block_number)
                .await
                .context("failed to fill witness")?,
        };
        WITNESS_VECTOR_GENERATOR_METRICS
            .artifact_download_time
            .observe(time.elapsed());

        let setup_data_key = ProverServiceDataKey {
            circuit_id: prover_job_metadata.circuit_id,
            round: prover_job_metadata.aggregation_round,
        }
        .crypto_setup_key();
        let prover_job = ProverJob::new(
            prover_job_metadata.block_number,
            prover_job_metadata.id,
            artifact,
            setup_data_key,
        );
        Ok(Some(prover_job))
    }

    async fn fill_witness(
        &self,
        circuit: ZkSyncBaseLayerCircuit,
        aux_data: CircuitAuxData,
        l1_batch_number: L1BatchNumber,
    ) -> anyhow::Result<CircuitWrapper> {
        if let ZkSyncBaseLayerCircuit::RAMPermutation(circuit_instance) = circuit {
            let sorted_witness_key = RamPermutationQueueWitnessKey {
                block_number: l1_batch_number,
                circuit_subsequence_number: aux_data.circuit_subsequence_number as usize,
                is_sorted: true,
            };
            let sorted_witness: RamPermutationQueueWitness = self
                .object_store
                .get(sorted_witness_key)
                .await
                .context("failed to load sorted witness key")?;

            let unsorted_witness_key = RamPermutationQueueWitnessKey {
                block_number: l1_batch_number,
                circuit_subsequence_number: aux_data.circuit_subsequence_number as usize,
                is_sorted: false,
            };
            let unsorted_witness: RamPermutationQueueWitness = self
                .object_store
                .get(unsorted_witness_key)
                .await
                .context("failed to load unsorted witness key")?;

            let mut witness = circuit_instance.witness.take().unwrap();
            witness.unsorted_queue_witness = FullStateCircuitQueueRawWitness {
                elements: unsorted_witness.witness.into(),
            };
            witness.sorted_queue_witness = FullStateCircuitQueueRawWitness {
                elements: sorted_witness.witness.into(),
            };
            circuit_instance.witness.store(Some(witness));

            return Ok(CircuitWrapper::Base(
                ZkSyncBaseLayerCircuit::RAMPermutation(circuit_instance),
            ));
        }
        Err(anyhow::anyhow!(
            "Unexpected circuit received with partial witness, expected RAM permutation, got {:?}",
            circuit.short_description()
        ))
    }

    async fn generate(
        &self,
        prover_job: ProverJob,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let start_time = Instant::now();
        let finalization_hints = self
            .finalization_hints
            .get(&prover_job.setup_data_key)
            .expect("no finalization hints for setup_data_key")
            .clone();
        let job_id = prover_job.job_id;
        let task = tokio::task::spawn_blocking(move || {
            let block_number = prover_job.block_number;
            let _span = tracing::info_span!("witness_vector_generator", %block_number).entered();
            Self::generate_witness_vector(prover_job, finalization_hints)
        });

        self.wait_for_task(job_id, start_time, task, cancellation_token.clone())
            .await?;

        tracing::info!(
            "Witness Vector Generator finished job {:?} in: {:?}",
            job_id,
            start_time.elapsed()
        );
        WITNESS_VECTOR_GENERATOR_METRICS
            .job_finished_time
            .observe(start_time.elapsed());
        Ok(())
    }

    #[tracing::instrument(
        skip_all,
        fields(l1_batch = % prover_job.block_number)
    )]
    pub fn generate_witness_vector(
        prover_job: ProverJob,
        finalization_hints: Arc<FinalizationHintsForProver>,
    ) -> anyhow::Result<WitnessVectorArtifactsTemp> {
        let time = Instant::now();
        let cs = match prover_job.circuit_wrapper.clone() {
            CircuitWrapper::Base(base_circuit) => {
                base_circuit.synthesis::<GoldilocksField>(&finalization_hints)
            }
            CircuitWrapper::Recursive(recursive_circuit) => {
                recursive_circuit.synthesis::<GoldilocksField>(&finalization_hints)
            }
            CircuitWrapper::BasePartial(_) => {
                return Err(anyhow::anyhow!(
                    "Invalid circuit wrapper received for witness vector generation"
                ));
            }
        };
        WITNESS_VECTOR_GENERATOR_METRICS
            .crypto_primitive_time
            .observe(time.elapsed());
        Ok(WitnessVectorArtifactsTemp::new(
            cs.witness.unwrap(),
            prover_job,
            time,
        ))
    }

    async fn wait_for_task(
        &self,
        job_id: u32,
        time: Instant,
        task: JoinHandle<anyhow::Result<WitnessVectorArtifactsTemp>>,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<()> {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                tracing::info!("Stop signal received, shutting down Witness Vector Generator...");
                return Ok(())
            }
            result = task => {
                let error_message = match result {
                    Ok(Ok(witness_vector)) => {
                        tracing::info!("Witness Vector Generator executed job {:?} in: {:?}", job_id, time.elapsed());
                        WITNESS_VECTOR_GENERATOR_METRICS.execution_time.observe(time.elapsed());
                        self
                            .save_result(witness_vector, job_id)
                            .await
                            .context("failed to save result")?;
                        return Ok(())
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => try_extract_panic_message(error),
                };
                tracing::error!("Witness Vector Generator failed on job {job_id:?} with error {error_message:?}");

                self.save_failure(job_id, error_message).await.context("failed to save result")?;
            }
        }

        Ok(())
    }

    async fn save_result(
        &self,
        artifacts: WitnessVectorArtifactsTemp,
        job_id: u32,
    ) -> anyhow::Result<()> {
        let time = Instant::now();
        self.sender
            .send(artifacts)
            .await
            .context("failed to send witness vector to prover")?;
        tracing::info!(
            "Witness Vector Generator sent job {:?} after {:?}",
            job_id,
            time.elapsed()
        );
        WITNESS_VECTOR_GENERATOR_METRICS
            .send_time
            .observe(time.elapsed());
        Ok(())
    }

    async fn save_failure(&self, job_id: u32, error: String) -> anyhow::Result<()> {
        self.connection_pool
            .connection()
            .await
            .context("failed to get connection")?
            .fri_prover_jobs_dal()
            .save_proof_error(job_id, error)
            .await;
        Ok(())
    }

    async fn backoff(&self, backoff: &mut Backoff, cancellation_token: CancellationToken) {
        let backoff_duration = backoff.delay();
        tracing::info!("Backing off for {:?}...", backoff_duration);
        // Error here corresponds to a timeout w/o receiving task cancel; we're OK with this.
        tokio::time::timeout(backoff_duration, cancellation_token.cancelled())
            .await
            .ok();
    }
}
