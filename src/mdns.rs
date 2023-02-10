use mdns_sd::{ServiceDaemon, ServiceInfo};

use uuid::Uuid;

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

pub fn get_service_instance_id(service_info: &ServiceInfo) -> Result<Uuid, ()> {
    let Some(instance_id) = service_info
        .get_fullname().split('.')
        .next()
        .and_then(|instance_id_str| Uuid::parse_str(instance_id_str).ok()) else {
            return Err(());
        };

    Ok(instance_id)
}
