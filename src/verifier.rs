use crate::challenges::Challenges;
use crate::composer::DeepCompositionCoeffs;
use crate::fri;
use crate::fri::FriVerifier;
use crate::hints::Hints;
use crate::merkle::MerkleProof;
use crate::merkle::MerkleTree;
use crate::merkle::MerkleTreeError;
use crate::random::PublicCoin;
use crate::utils::evaluate_vanishing_polynomial;
use crate::Air;
// use crate::channel::VerifierChannel;
use crate::Proof;
use ark_ff::Field;
use ark_ff::One;
use ark_ff::Zero;
use ark_poly::EvaluationDomain;
use ark_serialize::CanonicalSerialize;
use digest::Digest;
use digest::Output;
use rand::Rng;
use sha2::Sha256;
use std::ops::Deref;
use thiserror::Error;

/// Errors that are returned during verification of a STARK proof
#[derive(Error, Debug)]
pub enum VerificationError {
    #[error("constraint evaluations at the out-of-domain point are inconsistent")]
    InconsistentOodConstraintEvaluations,
    #[error("fri verification failed")]
    FriVerification(#[from] fri::VerificationError),
    #[error("query does not resolve to the base trace commitment")]
    BaseTraceQueryDoesNotMatchCommitment,
    #[error("query does not resolve to the extension trace commitment")]
    ExtensionTraceQueryDoesNotMatchCommitment,
    #[error("query does not resolve to the composition trace commitment")]
    CompositionTraceQueryDoesNotMatchCommitment,
    #[error("insufficient proof of work on fri commitments")]
    FriProofOfWork,
}

impl<A: Air> Proof<A> {
    pub fn verify(self) -> Result<(), VerificationError> {
        use VerificationError::*;

        let Proof {
            base_trace_commitment,
            extension_trace_commitment,
            composition_trace_commitment,
            ood_constraint_evaluations,
            ood_trace_states,
            trace_queries,
            trace_info,
            public_inputs,
            options,
            fri_proof,
            pow_nonce,
            ..
        } = self;

        let mut seed = Vec::new();
        public_inputs.serialize_compressed(&mut seed).unwrap();
        trace_info.serialize_compressed(&mut seed).unwrap();
        options.serialize_compressed(&mut seed).unwrap();
        let mut public_coin = PublicCoin::<Sha256>::new(&seed);

        let air = A::new(trace_info, public_inputs, options);

        let base_trace_comitment = Output::<Sha256>::from_iter(base_trace_commitment);
        public_coin.reseed(&base_trace_comitment.deref());
        let challenges = air.get_challenges(&mut public_coin);
        let hints = air.get_hints(&challenges);

        let extension_trace_commitment =
            extension_trace_commitment.map(|extension_trace_commitment| {
                let extension_trace_commitment =
                    Output::<Sha256>::from_iter(extension_trace_commitment);
                public_coin.reseed(&extension_trace_commitment.deref());
                extension_trace_commitment
            });

        let composition_coeffs = air.get_constraint_composition_coeffs(&mut public_coin);
        let composition_trace_commitment =
            Output::<Sha256>::from_iter(composition_trace_commitment);
        public_coin.reseed(&composition_trace_commitment.deref());

        let z = public_coin.draw::<A::Fq>();
        public_coin.reseed(&ood_trace_states.0);
        public_coin.reseed(&ood_trace_states.1);
        let calculated_ood_constraint_evaluation = ood_constraint_evaluation(
            composition_coeffs,
            &challenges,
            &hints,
            &ood_trace_states.0,
            &ood_trace_states.1,
            &air,
            z,
        );

        public_coin.reseed(&ood_constraint_evaluations);
        let mut acc = A::Fq::one();
        let provided_ood_constraint_evaluation =
            ood_constraint_evaluations
                .iter()
                .fold(A::Fq::zero(), |mut res, value| {
                    res += *value * acc;
                    acc *= z;
                    res
                });

        if calculated_ood_constraint_evaluation != provided_ood_constraint_evaluation {
            return Err(InconsistentOodConstraintEvaluations);
        }

        let deep_coeffs = air.get_deep_composition_coeffs(&mut public_coin);
        let fri_verifier = FriVerifier::<A::Fq, Sha256>::new(
            &mut public_coin,
            options.into_fri_options(),
            fri_proof,
            air.trace_len() - 1,
        )?;

        if options.grinding_factor != 0 {
            public_coin.reseed(&pow_nonce);
            if public_coin.seed_leading_zeros() < options.grinding_factor as u32 {
                return Err(FriProofOfWork);
            }
        }

        let mut rng = public_coin.draw_rng();
        let lde_domain_size = air.trace_len() * air.lde_blowup_factor();
        let query_positions = (0..options.num_queries)
            .map(|_| rng.gen_range(0..lde_domain_size))
            .collect::<Vec<usize>>();

        let base_trace_rows = trace_queries
            .base_trace_values
            .chunks(air.trace_info().num_base_columns)
            .collect::<Vec<&[A::Fp]>>();
        let extension_trace_rows = if air.trace_info().num_extension_columns > 0 {
            trace_queries
                .extension_trace_values
                .chunks(air.trace_info().num_extension_columns)
                .collect::<Vec<&[A::Fq]>>()
        } else {
            Vec::new()
        };

        let composition_trace_rows = trace_queries
            .composition_trace_values
            .chunks(air.ce_blowup_factor())
            .collect::<Vec<&[A::Fq]>>();

        // base trace positions
        verify_positions::<Sha256>(
            base_trace_comitment,
            &query_positions,
            &base_trace_rows,
            trace_queries.base_trace_proofs,
        )
        .map_err(|_| BaseTraceQueryDoesNotMatchCommitment)?;

        if let Some(extension_trace_commitment) = extension_trace_commitment {
            // extension trace positions
            verify_positions::<Sha256>(
                extension_trace_commitment,
                &query_positions,
                &extension_trace_rows,
                trace_queries.extension_trace_proofs,
            )
            .map_err(|_| ExtensionTraceQueryDoesNotMatchCommitment)?;
        }

        // composition trace positions
        verify_positions::<Sha256>(
            composition_trace_commitment,
            &query_positions,
            &composition_trace_rows,
            trace_queries.composition_trace_proofs,
        )
        .map_err(|_| CompositionTraceQueryDoesNotMatchCommitment)?;

        let deep_evaluations = deep_composition_evaluations(
            &air,
            &query_positions,
            deep_coeffs,
            base_trace_rows,
            extension_trace_rows,
            composition_trace_rows,
            z,
            ood_trace_states,
            ood_constraint_evaluations,
        );

        Ok(fri_verifier.verify(&query_positions, &deep_evaluations)?)
    }
}

fn ood_constraint_evaluation<A: Air>(
    mut composition_coefficients: Vec<(A::Fq, A::Fq)>,
    challenges: &Challenges<A::Fq>,
    hints: &Hints<A::Fq>,
    curr_trace_evals: &[A::Fq],
    next_trace_evals: &[A::Fq],
    air: &A,
    x: A::Fq,
) -> A::Fq {
    // TODO: refactor constraint and their divisors so they are grouped together
    let boundary_constraints = air.boundary_constraints();
    let transition_constraints = air.transition_constraints();
    let terminal_constraints = air.terminal_constraints();

    let boundary_divisor_degree = 1;
    let transition_divisor_degree = air.trace_len() - 1;
    let terminal_divisor_degree = 1;

    let trace_domain = air.trace_domain();
    let first_trace_x = A::Fp::one();
    let last_trace_x = trace_domain.group_gen_inv;
    // TODO docs
    let boundary_divisor = (x - A::Fq::from(first_trace_x)).inverse().unwrap();
    let terminal_divisor = (x - A::Fq::from(last_trace_x)).inverse().unwrap();
    let transition_divisor = (x - A::Fq::from(last_trace_x))
        * evaluate_vanishing_polynomial(&trace_domain, x)
            .inverse()
            .unwrap();

    // TODO: honestly I hate this
    let boundary_iter = boundary_constraints
        .iter()
        .map(|constraint| (constraint, boundary_divisor, boundary_divisor_degree));
    let transition_iter = transition_constraints
        .iter()
        .map(|constraint| (constraint, transition_divisor, transition_divisor_degree));
    let terminal_iter = terminal_constraints
        .iter()
        .map(|constraint| (constraint, terminal_divisor, terminal_divisor_degree));

    let mut result = A::Fq::zero();
    let trace_degree = air.trace_len() - 1;
    let composition_degree = air.composition_degree();
    for (constraint, divisor, divisor_degree) in
        boundary_iter.chain(transition_iter).chain(terminal_iter)
    {
        // TODO: proper errors
        let evaluation = constraint.evaluate(challenges, hints, curr_trace_evals, next_trace_evals);
        // TODO: consider better name here. Multiplying by divisor seems kinda retarded
        let quotient = evaluation * divisor;

        // TODO: don't allow degree 0 constraints
        let evaluation_degree = constraint.degree() * trace_degree - divisor_degree;
        assert!(evaluation_degree <= composition_degree);
        let degree_adjustment = (composition_degree - evaluation_degree) as u64;

        let (alpha, beta) = composition_coefficients.pop().unwrap();
        result += quotient * (alpha * x.pow([degree_adjustment]) + beta)
    }

    result
}

fn verify_positions<D: Digest>(
    commitment: Output<D>,
    positions: &[usize],
    rows: &[&[impl CanonicalSerialize]],
    proofs: Vec<MerkleProof>,
) -> Result<(), MerkleTreeError> {
    for ((position, proof), row) in positions.iter().zip(proofs).zip(rows) {
        let proof = proof.parse::<D>();
        let expected_leaf = &proof[0];
        let mut row_bytes = Vec::with_capacity(row.compressed_size());
        row.serialize_compressed(&mut row_bytes).unwrap();
        let actual_leaf = D::new_with_prefix(&row_bytes).finalize();

        if *expected_leaf != actual_leaf {
            return Err(MerkleTreeError::InvalidProof);
        }

        MerkleTree::<D>::verify(&commitment, &proof, *position)?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn deep_composition_evaluations<A: Air>(
    air: &A,
    query_positions: &[usize],
    composition_coeffs: DeepCompositionCoeffs<A::Fq>,
    base_trace_rows: Vec<&[A::Fp]>,
    extension_trace_rows: Vec<&[A::Fq]>,
    composition_trace_rows: Vec<&[A::Fq]>,
    z: A::Fq,
    ood_trace_states: (Vec<A::Fq>, Vec<A::Fq>),
    ood_constraint_evaluations: Vec<A::Fq>,
) -> Vec<A::Fq> {
    let trace_domain = air.trace_domain();
    let lde_domain = air.lde_domain();
    let xs = query_positions
        .iter()
        .map(|pos| lde_domain.element(*pos))
        .collect::<Vec<A::Fp>>();

    let mut evals = vec![A::Fq::zero(); query_positions.len()];

    // add base trace
    let next_z = z * &trace_domain.group_gen();
    for ((&x, row), eval) in xs.iter().zip(base_trace_rows).zip(&mut evals) {
        for (i, &val) in row.iter().enumerate() {
            let (alpha, beta, _) = composition_coeffs.base_trace[i];
            let t1 = (A::Fq::from(val) - ood_trace_states.0[i]) / (A::Fq::from(x) - z);
            let t2 = (A::Fq::from(val) - ood_trace_states.1[i]) / (A::Fq::from(x) - next_z);
            *eval += t1 * alpha + t2 * beta;
        }
    }

    // add extension trace
    let num_base_columns = air.trace_info().num_base_columns;
    for ((&x, row), eval) in xs.iter().zip(extension_trace_rows).zip(&mut evals) {
        for (i, &val) in row.iter().enumerate() {
            let (alpha, beta, _) = composition_coeffs.extension_trace[i];
            let t1 = (val - ood_trace_states.0[num_base_columns + i]) / (A::Fq::from(x) - z);
            let t2 = (val - ood_trace_states.1[num_base_columns + i]) / (A::Fq::from(x) - next_z);
            *eval += t1 * alpha + t2 * beta;
        }
    }

    // add composition trace
    let z_n = z.pow([air.ce_blowup_factor() as u64]);
    for ((&x, row), eval) in xs.iter().zip(composition_trace_rows).zip(&mut evals) {
        for (i, &value) in row.iter().enumerate() {
            let alpha = composition_coeffs.constraints[i];
            *eval += alpha * (value - ood_constraint_evaluations[i]) / (A::Fq::from(x) - z_n);
        }
    }

    // adjust degree
    let (alpha, beta) = composition_coeffs.degree;
    for (x, eval) in xs.iter().zip(&mut evals) {
        *eval *= alpha + beta * x;
    }

    evals
}
