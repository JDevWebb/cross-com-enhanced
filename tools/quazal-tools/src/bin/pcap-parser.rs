#![deny(clippy::pedantic)]

use std::fs::File;
use std::net::SocketAddr;
use std::net::SocketAddrV4;

use clap::Arg;
use clap::Command;
use etherparse::InternetSlice;
use pcap_parser::traits::PcapNGPacketBlock;
use pcap_parser::traits::PcapReaderIterator;
use pcap_parser::Block;
use pcap_parser::PcapBlockOwned;
use pcap_parser::PcapError;
use pcap_parser::PcapNGReader;
use quazal::prudp::packet::PacketFlag;
use quazal::prudp::packet::PacketType;
use quazal::prudp::packet::QPacket;
use quazal::rmc::Packet;
use quazal::Context;

fn main() {
    let matches = Command::new("pcap-parser")
        .arg(Arg::new("input").required(true))
        .arg(Arg::new("port").short('p').long("port").default_value("3074").value_parser(clap::value_parser!(u16)))
        .arg(Arg::new("crypto-key").short('c').long("crypto-key").default_value("CD&ML"))
        .arg(Arg::new("access-key").short('a').long("access-key").required(true))
        .get_matches();

    let file = File::open(matches.get_one::<String>("input").expect("input is required")).expect("Can't open file");

    let port: u16 = *matches.get_one::<u16>("port").expect("port has a default");
    let crypto_key: &str = matches.get_one::<String>("crypto-key").expect("crypto-key has a default");
    let access_key: &str = matches.get_one::<String>("access-key").expect("access-key is required");

    let mut reader = PcapNGReader::new(65536, file).expect("PcapNGReader");
    loop {
        match reader.next() {
            Ok((offset, block)) => {
                match block {
                    PcapBlockOwned::Legacy(_) => todo!(),
                    PcapBlockOwned::LegacyHeader(_) => todo!(),
                    PcapBlockOwned::NG(block) => match block {
                        Block::EnhancedPacket(epb) => {
                            parse(epb.packet_data(), port, crypto_key, access_key);
                        }
                        Block::SimplePacket(_) => todo!(),
                        Block::NameResolution(_) => todo!(),
                        Block::InterfaceStatistics(_) | Block::SectionHeader(_) | Block::InterfaceDescription(_) => {}
                        Block::SystemdJournalExport(_) => todo!(),
                        Block::DecryptionSecrets(_) => todo!(),
                        Block::Custom(_) => todo!(),
                        Block::Unknown(_) => todo!(),
                        Block::ProcessInformation(_) => todo!(),
                    },
                }
                reader.consume(offset);
            }
            Err(PcapError::Eof) => break,
            Err(PcapError::Incomplete(_)) => {
                reader.refill().unwrap();
            }
            Err(e) => panic!("error while reading: {e:?}"),
        }
    }
}

fn parse(data: &[u8], port: u16, crypto_key: &str, access_key: &str) -> bool {
    let packet = match etherparse::SlicedPacket::from_ethernet(data) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ether parse error: {e:?}");
            return false;
        }
    };
    if let Some(transport) = packet.transport {
        match transport {
            etherparse::TransportSlice::Udp(udp) => {
                if udp.source_port() == port || udp.destination_port() == port {
                    let (src, dst) = if let Some(InternetSlice::Ipv4(ip)) = packet.net {
                        let iph = ip.header();
                        (
                            SocketAddr::V4(SocketAddrV4::new(iph.source_addr(), udp.source_port())),
                            SocketAddr::V4(SocketAddrV4::new(iph.destination_addr(), udp.destination_port())),
                        )
                    } else {
                        panic!()
                    };
                    dump(udp.payload(), crypto_key, access_key, src, dst);
                    true
                } else {
                    false
                }
            }
            etherparse::TransportSlice::Tcp(_) | etherparse::TransportSlice::Icmpv6(_) | etherparse::TransportSlice::Icmpv4(_) => false,
        }
    } else {
        false
    }
}

fn dump(mut data: &[u8], crypto_key: &str, access_key: &str, src: SocketAddr, dst: SocketAddr) {
    #![allow(clippy::cast_possible_truncation)]

    let ctx = Context {
        access_key: access_key.as_bytes().to_owned(),
        crypto_key: crypto_key.as_bytes().to_owned(),
        ..Default::default()
    };

    let mut packet_data: &[u8];

    while !data.is_empty() {
        let (prudp, size) = match QPacket::from_bytes(&ctx, data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{src} -> {dst}] PRUDP Parsing error: {e:?}");
                eprintln!("data = {data:x?}");
                break;
            }
        };
        (packet_data, data) = data.split_at(size as usize);
        print!(
            "[{} -> {}; {}] {}>{} {:?} ({}) {:?} {}",
            src,
            dst,
            prudp.sequence,
            prudp.source.port,
            prudp.destination.port,
            prudp.destination.stream_type,
            prudp.flags.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join("|"),
            prudp.packet_type,
            if prudp.validate(&ctx, packet_data).is_ok() { "valid" } else { "invalid" }
        );

        if matches!(prudp.packet_type, PacketType::Data) && !prudp.flags.contains(PacketFlag::Ack) && prudp.fragment_id.unwrap_or_default() == 0 {
            if let Some(0) = prudp.fragment_id {
                let rmc = match Packet::from_bytes(&prudp.payload) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[{src} -> {dst}] RMC Parsing error: {e:?}");
                        eprintln!("prudp.payload = {:x?}", prudp.payload);
                        break;
                    }
                };
                match rmc {
                    Packet::Request(req) => {
                        print!(" {}.{} {}", req.protocol_id, req.method_id, req.call_id);
                    }
                    Packet::Response(_) => {}
                }
            }
        }
        println!();
    }
}
