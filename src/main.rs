use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter,
};
use btleplug::platform::{Manager, Peripheral, PeripheralId};
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, Write};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio::task::JoinHandle;
use sysinfo::System;

mod flipper_manager;
mod helpers;
mod system_info;

async fn data_sender(flipper: Peripheral) {
    let chars = flipper.characteristics();
    let cmd_char = match chars
        .iter()
        .find(|c| c.uuid == flipper_manager::FLIPPER_CHARACTERISTIC_UUID)
    {
        Some(c) => c,
        None => {
            return println!("Failed to find characteristic");
        }
    };
    println!("Now you can launch PC Monitor app on your Flipper");

    // Initialize system info and track last successful send time
    let mut system_info = sysinfo::System::new_all();
    let mut last_successful_send = Instant::now();
    loop {
        let systeminfo = system_info::SystemInfo::get_system_info(&mut system_info).await;
        let systeminfo_bytes = bincode::serialize(&systeminfo).unwrap();

        match flipper
            .write(
                cmd_char,
                &systeminfo_bytes,
                btleplug::api::WriteType::WithoutResponse,
            )
            .await
        {
            Ok(_) => {
                last_successful_send = Instant::now();
            }
            Err(e) => {
                println!("Failed to write to Flipper: {:?}", e);
                // If no successful send in the last 10 seconds, attempt to reconnect
                if last_successful_send.elapsed() > Duration::from_secs(10) {
                    println!("Connection seems unstable. Attempting to reconnect...");
                    if let Err(e) = flipper.connect().await {
                        println!("Reconnection failed: {:?}", e);
                    } else {
                        println!("Reconnected successfully");
                    }
                }
            }
        }

        // Wait for 1 second before sending the next update
        sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let manager = Manager::new().await?;

    let central = flipper_manager::get_central(&manager).await;
    println!("Found {:?} adapter", central.adapter_info().await.unwrap());
    println!();

    let mut events = central.events().await?;
    let mut flipper_name = String::new();
    println!("- Scan will be searching for Flippers with a name that contains the string you enter here");
    println!("- If you run official firmware you should be fine by entering 'Flipper' (case sensitive)");
    println!("- Empty string will search for all possible Flippers (experimental)");
    println!();
    print!("Enter the name (for empty just press Enter): ");
    io::stdout().flush().unwrap();
    io::stdin().read_line(&mut flipper_name).expect("Error: unable to read user input");
    let flipper_name = flipper_name.trim();
    println!();

    println!("Scanning...");
    central.start_scan(ScanFilter::default()).await?;

    let mut workers: HashMap<PeripheralId, JoinHandle<()>> = HashMap::new();
    let mut last_connection_attempt: HashMap<PeripheralId, Instant> = HashMap::new();

    while let Some(event) = events.next().await {
        match event {
            CentralEvent::DeviceDiscovered(id) => {
                // Check if we've recently attempted to connect to this device
                if let Some(last_attempt) = last_connection_attempt.get(&id) {
                    if last_attempt.elapsed() < Duration::from_secs(30) {
                        continue; // Skip reconnection attempts within 30 seconds
                    }
                }
                if let Some(flp) = flipper_manager::get_flipper(&central, &id, (&flipper_name).to_string()).await {
                    println!("Connecting to Flipper {}", &id.to_string());
                    match flp.connect().await {
                        Ok(_) => {
                            println!("Connected to Flipper {}", &id.to_string());
                            last_connection_attempt.insert(id.clone(), Instant::now());
                        }
                        Err(e) => {
                            println!("Failed to connect to Flipper {}: {:?}", id.to_string(), e);
                            last_connection_attempt.insert(id.clone(), Instant::now());
                        }
                    }
                }
            }
            CentralEvent::DeviceConnected(id) => {
                if let Some(flp) = flipper_manager::get_flipper(&central, &id, (&flipper_name).to_string()).await {
                    flp.discover_services().await?;
                    println!("Services discovered for Flipper {}", &id.to_string());

                    // Spawn a new task for data sending
                    workers.insert(id, tokio::spawn(data_sender(flp)));
                };
            }
            CentralEvent::DeviceDisconnected(id) => {
                if let Some(worker) = workers.get(&id) {
                    worker.abort();
                    println!("Disconnected from Flipper {}", &id.to_string());
                    workers.remove(&id);

                    // Attempt automatic reconnection
                    if let Some(flp) = flipper_manager::get_flipper(&central, &id, (&flipper_name).to_string()).await {
                        println!("Attempting to reconnect to Flipper {}", &id.to_string());
                        tokio::spawn(async move {
                            sleep(Duration::from_secs(5)).await; // Wait for 5 seconds before attempting to reconnect
                            if let Err(e) = flp.connect().await {
                                println!("Failed to reconnect to Flipper {}: {:?}", id.to_string(), e);
                            } else {
                                println!("Successfully reconnected to Flipper {}", id.to_string());
                            }
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
