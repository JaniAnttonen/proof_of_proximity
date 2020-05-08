use std::str::FromStr;
use std::sync::mpsc::{Sender, channel, Receiver};
use std::{thread, time};
use std::error::Error;
use std::fmt;
use ramp::Int;
use rand_core::RngCore;
use sha3::{Digest, Sha3_512};

mod vdf;
pub mod util; 
pub mod primality;

pub const RSA_2048: &str = "135066410865995223349603216278805969938881475605667027524485143851526510604859533833940287150571909441798207282164471551373680419703964191743046496589274256239341020864383202110372958725762358509643110564073501508187510676594629205563685529475213500852879416377328533906109750544334999811150056977236890927563";


// rsa_mod = N, seed = g
fn main() {
    // This parameter just needs to provide a group of unknown order, thus a large RSA number is
    // required. N in the paper.
    let rsa_mod = Int::from_str(RSA_2048).unwrap();

    // Security parameter, g in the paper. This needs to be replaced with a key that's decided
    // between two peers with Diffie-Hellman. The starting point for the VDF that gets squared
    // repeatedly for T times. Used to verify that the calculations started here. That's why the
    // setup needs to generate a random starting point that couldn't have been forged beforehand.
    let seed = hash(&format!("Beep boop beep"), &rsa_mod);

    let proof_of_latency = vdf::ProofOfLatency{rsa_mod, seed, upper_bound: 6537892};
    
    // OH YES, it's a random prime that gets used in the proof and verification. This has to be
    // sent from another peer and this actually is the thing that ends the calculation and
    // facilitates the proof.
    let cap: u128 = get_prime().into();

    // Run the VDF, returning connection channels to push to and receive data from
    let (vdf_worker, worker_output) = proof_of_latency.run_vdf_worker();

    // Sleep for 300 milliseconds to simulate latency overseas
    let sleep_time = time::Duration::from_millis(300);
    thread::sleep(sleep_time);

    // Send received signature from the other peer, "capping off" the 
    vdf_worker.send(cap).unwrap();

    // Wait for response from VDF worker
    let response = worker_output.recv().unwrap().unwrap();
   
    println!("VDF ran for {:?} times!", response.output.iterations);
    println!("The output being {:?}", response.output.result);

    // Verify the proof
    let is_ok = response.verify();

    match is_ok {
        true => println!("The VDF is correct!"),
        false => println!("The VDF couldn't be verified!"),
    }
}

