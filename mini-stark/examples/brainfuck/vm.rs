use crate::tables::BrainfuckColumn;
use crate::tables::InputBaseColumn;
use crate::tables::InstructionBaseColumn;
use crate::tables::MemoryBaseColumn;
use crate::tables::OutputBaseColumn;
use crate::tables::ProcessorBaseColumn;
use crate::trace::into_columns;
use crate::BrainfuckTrace;
use ark_ff::Field;
use ark_ff::One;
use ark_ff::Zero;
// use ark_ff_optimized::fp64::Fp;
use fast_poly::allocator::PageAlignedAllocator;
use mini_stark::Matrix;

type Fp = <BrainfuckTrace as mini_stark::Trace>::Fp;

/// Opcodes determined by the lexer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    IncrementPointer = b'>' as isize,
    DecrementPointer = b'<' as isize,
    Increment = b'+' as isize,
    Decrement = b'-' as isize,
    Write = b'.' as isize,
    Read = b',' as isize,
    LoopBegin = b'[' as isize,
    LoopEnd = b']' as isize,
}

impl OpCode {
    // TODO: probably not idiomatic
    pub const VALUES: [OpCode; 8] = [
        OpCode::IncrementPointer,
        OpCode::DecrementPointer,
        OpCode::Increment,
        OpCode::Decrement,
        OpCode::Write,
        OpCode::Read,
        OpCode::LoopBegin,
        OpCode::LoopEnd,
    ];
}

/// Lexer turns the source code into a sequence of opcodes
fn lex(source: &str) -> Vec<OpCode> {
    let mut operations = Vec::new();

    for symbol in source.chars() {
        let op = match symbol {
            '>' => Some(OpCode::IncrementPointer),
            '<' => Some(OpCode::DecrementPointer),
            '+' => Some(OpCode::Increment),
            '-' => Some(OpCode::Decrement),
            '.' => Some(OpCode::Write),
            ',' => Some(OpCode::Read),
            '[' => Some(OpCode::LoopBegin),
            ']' => Some(OpCode::LoopEnd),
            _ => None,
        };

        // Non-opcode characters are comments
        if let Some(op) = op {
            operations.push(op);
        }
    }

    operations
}

pub fn compile(source: &str) -> Vec<usize> {
    let opcodes = lex(source);
    let mut program = Vec::new();
    let mut stack = Vec::new();
    for opcode in opcodes.into_iter() {
        program.push(opcode as usize);
        match opcode {
            OpCode::LoopBegin => {
                // Placeholder for position of loop end
                program.push(0);
                stack.push(program.len() - 1);
            }
            OpCode::LoopEnd => {
                let last = stack.pop().expect("loop has no beginning");
                program.push(last + 1); // loop end
                program[last] = program.len(); // loop beginning
            }
            _ => (),
        }
    }
    program
}

/// Registers of the brainfuck VM
#[derive(Default)]
struct Register {
    /// Cycle
    cycle: usize,
    /// Instruction pointer
    ip: usize,
    /// Current instruction
    curr_instr: usize,
    /// Next instruction
    next_instr: usize,
    /// Memory pointer
    mp: usize,
    /// Memory value
    mem_val: usize,
}

// Outputs base execution trace
pub fn simulate(
    program: &[usize],
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
) -> BrainfuckTrace {
    let mut tape = [0u8; 1024];
    let mut register = Register::default();
    register.curr_instr = program[0];
    register.next_instr = if program.len() == 1 { 0 } else { program[1] };

    // execution trace tables in row major
    let mut processor_rows = Vec::new();
    let mut instruction_rows = Vec::new();
    let mut input_rows = Vec::new();
    let mut output_rows = Vec::new();
    let mut memory_rows = Vec::new();

    for i in 0..program.len() {
        instruction_rows.push(vec![
            Fp::from(i as u64),
            Fp::from(program[i] as u64),
            Fp::from(program.get(i + 1).map_or(0, |&x| x as u64)),
        ])
    }

    // main loop
    while register.ip < program.len() {
        let mem_val = Fp::from(register.mem_val as u64);

        println!("Cycle: {}", register.cycle);

        processor_rows.push(vec![
            Fp::from(register.cycle as u64),
            Fp::from(register.ip as u64),
            Fp::from(register.curr_instr as u64),
            Fp::from(register.next_instr as u64),
            Fp::from(register.mp as u64),
            mem_val,
            mem_val.inverse().unwrap_or_else(Fp::zero),
        ]);

        instruction_rows.push(vec![
            Fp::from(register.ip as u64),
            Fp::from(register.curr_instr as u64),
            Fp::from(register.next_instr as u64),
        ]);

        // Update pointer registers according to instruction
        if register.curr_instr == OpCode::LoopBegin as usize {
            register.ip = if register.mem_val == 0 {
                program[register.ip + 1]
            } else {
                register.ip + 2
            };
        } else if register.curr_instr == OpCode::LoopEnd as usize {
            register.ip = if register.mem_val != 0 {
                program[register.ip + 1]
            } else {
                register.ip + 2
            }
        } else if register.curr_instr == OpCode::DecrementPointer as usize {
            register.ip += 1;
            register.mp -= 1;
        } else if register.curr_instr == OpCode::IncrementPointer as usize {
            register.ip += 1;
            register.mp += 1;
        } else if register.curr_instr == OpCode::Increment as usize {
            register.ip += 1;
            tape[register.mp] += 1;
        } else if register.curr_instr == OpCode::Decrement as usize {
            register.ip += 1;
            tape[register.mp] -= 1;
        } else if register.curr_instr == OpCode::Write as usize {
            register.ip += 1;
            let x = &tape[register.mp..register.mp + 1];
            output.write_all(x).expect("failed to write output");
            output_rows.push(vec![x[0].into()]);
        } else if register.curr_instr == OpCode::Read as usize {
            register.ip += 1;
            let mut x = [0u8; 1];
            input.read_exact(&mut x).expect("failed to read input");
            tape[register.mp] = x[0];
            input_rows.push(vec![x[0].into()])
        } else {
            panic!("unrecognized instruction at ip:{}", register.ip);
        }

        register.cycle += 1;
        register.curr_instr = program.get(register.ip).map_or(0, |&x| x);
        register.next_instr = program.get(register.ip + 1).map_or(0, |&x| x);
        register.mem_val = tape[register.mp].into(); // TODO: Change to u8
    }

    // Collect final state into execution tables
    let mem_val = Fp::from(register.mem_val as u64);
    processor_rows.push(vec![
        Fp::from(register.cycle as u64),
        Fp::from(register.ip as u64),
        Fp::from(register.curr_instr as u64),
        Fp::from(register.next_instr as u64),
        Fp::from(register.mp as u64),
        mem_val,
        mem_val.inverse().unwrap(),
    ]);

    instruction_rows.push(vec![
        Fp::from(register.ip as u64),
        Fp::from(register.curr_instr as u64),
        Fp::from(register.next_instr as u64),
    ]);

    // sort instructions by address
    instruction_rows.sort_by_key(|row| row[0]);

    memory_rows = derive_memory_rows(&processor_rows);

    let padding_len = {
        let max_length = [
            processor_rows.len(),
            memory_rows.len(),
            instruction_rows.len(),
            input_rows.len(),
            output_rows.len(),
        ]
        .into_iter()
        .max()
        .unwrap();
        ceil_power_of_two(max_length)
    };

    pad_processor_rows(&mut processor_rows, padding_len);
    pad_memory_rows(&mut memory_rows, padding_len);
    pad_instruction_rows(&mut instruction_rows, padding_len);
    pad_input_rows(&mut input_rows, padding_len);
    pad_output_rows(&mut output_rows, padding_len);

    let processor_base_trace = Matrix::new(into_columns(processor_rows));
    let memory_base_trace = Matrix::new(into_columns(memory_rows));
    let instruction_base_trace = Matrix::new(into_columns(instruction_rows));
    let input_base_trace = Matrix::new(into_columns(input_rows));
    let output_base_trace = Matrix::new(into_columns(output_rows));

    BrainfuckTrace::new(
        processor_base_trace,
        memory_base_trace,
        instruction_base_trace,
        input_base_trace,
        output_base_trace,
    )
}

fn pad_processor_rows(rows: &mut Vec<Vec<Fp>>, n: usize) {
    use ProcessorBaseColumn::*;
    while rows.len() < n {
        let last_row = rows.last().unwrap();
        let mut new_row = vec![Fp::zero(); last_row.len()];
        new_row[Cycle as usize] = last_row[Cycle as usize] + Fp::one();
        new_row[Ip as usize] = last_row[Ip as usize];
        new_row[CurrInstr as usize] = Fp::zero();
        new_row[NextInstr as usize] = Fp::zero();
        new_row[Mp as usize] = last_row[Mp as usize];
        new_row[MemVal as usize] = last_row[MemVal as usize];
        new_row[MemValInv as usize] = last_row[MemValInv as usize];
        rows.push(new_row);
    }
}

// TODO: could move pad functions to trace.rs
fn pad_memory_rows(rows: &mut Vec<Vec<Fp>>, n: usize) {
    use MemoryBaseColumn::*;
    while rows.len() < n {
        let last_row = rows.last().unwrap();
        let mut new_row = vec![Fp::zero(); last_row.len()];
        new_row[Cycle as usize] = last_row[Cycle as usize] + Fp::one();
        new_row[Mp as usize] = last_row[Mp as usize];
        new_row[MemVal as usize] = last_row[MemVal as usize];
        new_row[Dummy as usize] = Fp::one();
        rows.push(new_row);
    }
}

fn pad_instruction_rows(rows: &mut Vec<Vec<Fp>>, n: usize) {
    use InstructionBaseColumn::*;
    let last_ip = rows.last().unwrap()[Ip as usize];
    while rows.len() < n {
        let mut new_row = vec![Fp::zero(); InstructionBaseColumn::NUM_TRACE_COLUMNS];
        new_row[Ip as usize] = last_ip;
        new_row[CurrInstr as usize] = Fp::zero();
        new_row[NextInstr as usize] = Fp::zero();
        rows.push(new_row);
    }
}

fn pad_input_rows(rows: &mut Vec<Vec<Fp>>, n: usize) {
    while rows.len() < n {
        let new_row = vec![Fp::zero(); InputBaseColumn::NUM_TRACE_COLUMNS];
        rows.push(new_row);
    }
}

fn pad_output_rows(rows: &mut Vec<Vec<Fp>>, n: usize) {
    while rows.len() < n {
        let new_row = vec![Fp::zero(); OutputBaseColumn::NUM_TRACE_COLUMNS];
        rows.push(new_row);
    }
}

fn derive_memory_rows(processor_rows: &[Vec<Fp>]) -> Vec<Vec<Fp>> {
    // copy unpadded rows and sort
    // TODO: sorted by IP and then CYCLE. Check to see if processor table sorts by
    // cycle.
    use MemoryBaseColumn::*;
    let mut memory_rows = processor_rows
        .iter()
        .filter_map(|row| {
            if row[ProcessorBaseColumn::CurrInstr as usize].is_zero() {
                None
            } else {
                let mut mem_row = vec![Fp::zero(); MemoryBaseColumn::NUM_TRACE_COLUMNS];
                mem_row[Cycle as usize] = row[ProcessorBaseColumn::Cycle as usize];
                mem_row[Mp as usize] = row[ProcessorBaseColumn::Mp as usize];
                mem_row[MemVal as usize] = row[ProcessorBaseColumn::MemVal as usize];
                mem_row[Dummy as usize] = Fp::zero();
                Some(mem_row)
            }
        })
        .collect::<Vec<Vec<Fp>>>();

    memory_rows.sort_by_key(|row| (row[Mp as usize], row[Cycle as usize]));

    // insert dummy rows for smooth clk jumps
    let mut i = 0;
    while i < memory_rows.len() - 1 {
        let curr = &memory_rows[i];
        let next = &memory_rows[i + 1];

        // // check sorted by memory address then cycle
        // if curr_row[Mp as usize] == next[Mp as usize] {
        //     // assert!(curr_row[Self::CYCLE] == next[Self::CYCLE] -
        //     // Fp::BasePrimeField::one())
        // }

        if curr[Mp as usize] == next[Mp as usize]
            && curr[Cycle as usize] + Fp::one() != next[Cycle as usize]
        {
            memory_rows.insert(
                i + 1,
                vec![
                    curr[Cycle as usize] + Fp::one(),
                    curr[Mp as usize],
                    curr[MemVal as usize],
                    Fp::one(), // dummy=yes
                ],
            )
        }

        i += 1;
    }

    memory_rows
}

/// Rounds the input value up the the nearest power of two
fn ceil_power_of_two(value: usize) -> usize {
    if value.is_power_of_two() {
        value
    } else {
        value.next_power_of_two()
    }
}
