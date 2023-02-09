use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::time::timeout as tokio_timeout;
use uuid::Uuid;

use std::time::Duration;
use std::{collections::HashMap, net::Ipv4Addr};

use crate::networking::my_ipv4_addrs;

/// Advertise a service with the given name over MDNS.
pub fn advertise_service(
    service_name: &str,
    port: u16,
    instance_id: &Uuid,
    props: Option<HashMap<String, String>>,
) -> Result<(), ()> {
    let mdns = ServiceDaemon::new().unwrap();

    // TODO: enumerate and include IPv6 addresses
    let my_addrs: Vec<Ipv4Addr> = my_ipv4_addrs();

    let service_domain = format!("_{service_name}._tcp.local.");
    let service_hostname = format!("{service_name}.local.");

    println!("Advertising {service_name}; instance_id={instance_id} domain={service_domain} port={port} props={props:?}");

    // Register our service
    let service_info = ServiceInfo::new(
        &service_domain,
        &instance_id.to_string(),
        &service_hostname,
        &my_addrs[..],
        port,
        props,
    )
    .unwrap();

    mdns.register(service_info).unwrap();

    Ok(())
}

pub async fn discover_service(service_name: &str) -> Result<ServiceInfo, ()> {
    discover_service_with_timeout(service_name, Duration::from_secs(30)).await
}

pub async fn discover_service_with_timeout(
    service_name: &str,
    timeout_duration: Duration,
) -> Result<ServiceInfo, ()> {
    let mdns = ServiceDaemon::new().unwrap();
    let service_type = format!("{service_name}._tcp.local.");
    let receiver = mdns.browse(&service_type).unwrap();
    println!("Attempting discovery of {service_type}");

    // note: we could distinguish between "not found because `receiver` closed its channel and
    // stopped sending us events" and "not found because `max_wait` has elapsed", but it doesn't
    // seem obviously to be worth bothering with

    let discover_service = async {
        while let Ok(event) = receiver.recv_async().await {
            let ServiceEvent::ServiceResolved(info) = event else {
                // We don't care about other events here
                continue;
            };
            if info.get_addresses().is_empty() {
                // This should never happen, but let's check here so all consumer code can just
                // info.get_addresses().get(0).upwrap() without needing to worry about it exploding.
                continue;
            }
            // tell mdns to stop browsing and consume its SearchStopped message, otherwise we'll get
            // a "sending on a closed channel" error in the console when mdns goes out of scope
            let _ = mdns.stop_browse(&service_type);
            while let Ok(event) = receiver.recv() {
                if matches!(event, ServiceEvent::SearchStopped(_)) {
                    break;
                }
            }

            return Ok(info);
        }
        Err(())
    };

    let Ok(resp) = tokio_timeout(timeout_duration, discover_service).await else {
        return Err(());
    };

    resp
}

pub fn get_service_instance_id(service_info: &ServiceInfo) -> Result<Uuid, ()> {
    let Some(instance_id) = service_info
        .get_fullname().split('.')
        .next()
        .and_then(|instance_id_str| Uuid::parse_str(instance_id_str).ok()) else {
            return Err(());
        };

    Ok(instance_id)
}
