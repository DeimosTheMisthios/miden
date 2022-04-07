use miden::{ProgramInputs, ProofOptions, Script};
use structopt::StructOpt;

pub mod fibonacci;

// EXAMPLE
// ================================================================================================

pub struct Example {
    pub program: Script,
    pub inputs: ProgramInputs,
    pub pub_inputs: Vec<u64>,
    pub num_outputs: usize,
    pub expected_result: Vec<u64>,
}

// EXAMPLE OPTIONS
// ================================================================================================

#[derive(StructOpt, Debug)]
#[structopt(name = "Miden", about = "Miden examples")]
pub struct ExampleOptions {
    #[structopt(subcommand)]
    pub example: ExampleType,

    /// Security level for execution proofs generated by the VM
    #[structopt(short = "s", long = "security", default_value = "96bits")]
    security: String,
}

impl ExampleOptions {
    pub fn get_proof_options(&self) -> ProofOptions {
        match self.security.as_str() {
            "96bits" => ProofOptions::with_96_bit_security(),
            "128bits" => ProofOptions::with_128_bit_security(),
            other => panic!("{} is not a valid security level", other),
        }
    }
}

#[derive(StructOpt, Debug)]
//#[structopt(about = "available examples")]
pub enum ExampleType {
    /// Compute a Fibonacci sequence of the specified length
    Fib {
        /// Length of Fibonacci sequence
        #[structopt(short = "n", default_value = "1024")]
        sequence_length: usize,
    },
}

// TESTS
// ================================================================================================

#[cfg(test)]
pub fn test_example(example: Example, fail: bool) {
    let Example {
        program,
        inputs,
        pub_inputs,
        num_outputs,
        expected_result,
    } = example;

    let options = ProofOptions::new(
        32,
        8,
        0,
        miden::HashFunction::Blake3_256,
        miden::FieldExtension::None,
        8,
        256,
    );

    let (mut outputs, proof) = miden::execute(&program, &inputs, num_outputs, &options).unwrap();

    assert_eq!(
        expected_result, outputs,
        "Program result was computed incorrectly"
    );

    if fail {
        outputs[0] = outputs[0] + 1;
        assert!(miden::verify(*program.hash(), &pub_inputs, &outputs, proof).is_err())
    } else {
        assert!(miden::verify(*program.hash(), &pub_inputs, &outputs, proof).is_ok());
    }
}