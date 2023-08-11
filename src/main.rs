use std::time::Duration;

use btleplug::{
    api::{Central, Manager as _, Peripheral as _, ScanFilter, Service, WriteType},
    platform::{Adapter, Manager, Peripheral},
};
use eyre::{bail, Result};
use futures::stream::StreamExt;
use tokio::time;

const SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_bytes([
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x19, 0x10,
]);

const CMD_INIT_SESSION: &[u8] = &[
    0x4c, 0x54, 0x09, 0x00, 0x00, 0x53, 0x00, 0x00, 0x01, 0x00, 0x94, 0x74,
];
const CMD_LIGHT_ON: &[u8] = &[
    0x4c, 0x54, 0x09, 0x00, 0x30, 0x57, 0x00, 0x00, 0x01, 0x01, 0x22, 0xdf,
];
const CMD_LIGHT_OFF: &[u8] = &[
    0x4c, 0x54, 0x09, 0x00, 0x30, 0x57, 0x00, 0x00, 0x01, 0x00, 0x32, 0xfe,
];

#[tokio::main]
async fn main() -> Result<()> {
    let manager = Manager::new().await?;

    let adapters = manager.adapters().await?;
    let central = adapters.into_iter().nth(0).unwrap();

    central.start_scan(ScanFilter::default()).await?;

    let led = loop {
        if let Ok(led) = find_led(&central).await {
            break led;
        }
        time::sleep(Duration::from_millis(100)).await;
    };

    led.connect().await?;

    dbg!("connected");

    let service = find_service(&led).await?;

    let characteristic = service.characteristics.iter().next().unwrap();

    led.subscribe(characteristic).await?;

    let mut notifications = led.notifications().await.unwrap();

    tokio::spawn(async move {
        while let Some(notification) = notifications.next().await {
            eprintln!("NOTIFY {:?} - {:?}", notification.uuid, notification.value);
        }
    });

    // led.write(characteristic, CMD_INIT_SESSION, WriteType::WithoutResponse)
    //     .await?;

    loop {
        for cmd in [CMD_LIGHT_ON, CMD_LIGHT_OFF] {
            led.write(characteristic, cmd, WriteType::WithoutResponse)
                .await?;
            time::sleep(Duration::from_secs(1)).await;
        }
    }
}

async fn find_service(led: &Peripheral) -> Result<Service> {
    led.discover_services().await?;

    for service in led.services() {
        if service.uuid == SERVICE_UUID {
            return Ok(service);
        }
    }

    bail!("didn't find service");
}

async fn find_led(central: &Adapter) -> Result<Peripheral> {
    for p in central.peripherals().await.unwrap() {
        let local_name = p.properties().await.unwrap().unwrap().local_name;

        if let Some(name) = local_name {
            if name == "BT_LED" {
                return Ok(p);
            }
        }
    }

    bail!("didn't find a thing")
}
