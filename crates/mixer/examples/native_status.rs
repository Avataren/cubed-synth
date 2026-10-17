//! Opens the default output device, lets the mixer run silently for a
//! second, and prints the host's status: a check that the device works.
//!
//! cargo run -p cubed-mixer --features native --example native_status

use std::time::Duration;

use cubed_mixer::native::{HostConfig, NativeHost};
use cubed_mixer::BusId;

fn main() -> Result<(), String> {
    let mut host = NativeHost::open(HostConfig::default())?;
    println!("{} at {} Hz", host.device_name(), host.sample_rate());
    std::thread::sleep(Duration::from_secs(1));
    host.poll(|event| println!("event {event:?}"), |_| {});
    println!("{:#?}", host.status());
    println!("master meter {:?}", host.meter(BusId::MASTER));
    Ok(())
}
