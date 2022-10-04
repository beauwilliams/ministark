use crate::channel::ProverChannel;
use crate::random::PublicCoin;
use crate::Air;
use crate::Matrix;
use crate::Trace;
use crate::TraceInfo;
use ark_poly::domain::Radix2EvaluationDomain;
use ark_poly::EvaluationDomain;
use ark_serialize::CanonicalDeserialize;
use ark_serialize::CanonicalSerialize;
use fast_poly::GpuField;
use sha2::Sha256;

// TODO: include ability to specify:
// - base field
// - extension field
// - hashing function
// - determine if grinding factor is appropriate
// - fri folding factor
// - fri max remainder size
#[derive(Debug, Clone, Copy, CanonicalSerialize, CanonicalDeserialize)]
pub struct ProofOptions {
    pub num_queries: u8,
    pub expansion_factor: u8,
}

impl ProofOptions {
    pub fn new(num_queries: u8, expansion_factor: u8) -> Self {
        ProofOptions {
            num_queries,
            expansion_factor,
        }
    }
}

/// A proof generated by a mini-stark prover
#[derive(Debug, Clone)]
pub struct Proof {
    options: ProofOptions,
    trace_info: TraceInfo,
    commitments: Vec<u64>,
}

/// Errors that can occur during the proving stage
#[derive(Debug)]
pub enum ProvingError {
    // /// This error occurs when a transition constraint evaluated over a specific execution trace
    // /// does not evaluate to zero at any of the steps.
    // UnsatisfiedTransitionConstraintError(usize),
    // /// This error occurs when polynomials built from the columns of a constraint evaluation
    // /// table do not all have the same degree.
    // MismatchedConstraintPolynomialDegree(usize, usize),
}

pub trait Prover {
    type Fp: GpuField;
    type Air: Air<Fp = Self::Fp>;
    type Trace: Trace<Fp = Self::Fp>;

    fn new(options: ProofOptions) -> Self;

    fn get_pub_inputs(&self, trace: &Self::Trace) -> <Self::Air as Air>::PublicInputs;

    fn options(&self) -> ProofOptions;

    fn build_trace_commitment(
        &self,
        trace: &Matrix<Self::Fp>,
        domain: Radix2EvaluationDomain<Self::Fp>,
    ) -> Matrix<Self::Fp> {
        let trace_lde = trace.evaluate(domain);
        trace_lde
    }

    fn generate_proof(&self, trace: Self::Trace) -> Result<Proof, ProvingError> {
        let options = self.options();
        let trace_info = trace.info();
        let pub_inputs = self.get_pub_inputs(&trace);
        let air = Self::Air::new(trace_info.clone(), pub_inputs, options);
        let channel = ProverChannel::<Self::Air, Sha256>::new(&air);

        let base_columns = trace.base_columns();
        let base_polynomials = base_columns.interpolate_columns();

        Ok(Proof {
            options,
            trace_info,
            commitments: Vec::new(),
        })
    }
}