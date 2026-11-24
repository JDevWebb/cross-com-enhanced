#![deny(clippy::pedantic)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use clap::Arg;
use clap::ArgAction;
use clap::Command;
use nom::AsBytes;
use quazal_tools::generate::build_import_map;
use quazal_tools::generate::generate_source;
use quazal_tools::generate::write_modules;
use quazal_tools::parse::parse_ddl;
use quazal_tools::parse::Element;
use quazal_tools::parse::DEBUG;

const MAGIC: u32 = 0xCD65_2312;

#[allow(clippy::too_many_lines)]
fn main() {
    let matches = Command::new("DDL Binary parser")
        .arg(Arg::new("verbose").long("verbose").short('v').action(ArgAction::SetTrue))
        .arg(Arg::new("debug").long("debug").short('d').action(ArgAction::SetTrue))
        .arg(Arg::new("output").long("output").short('o'))
        .arg(Arg::new("generate").long("generate").short('g'))
        .arg(Arg::new("id_mapping").long("ids").short('i'))
        .arg(Arg::new("binary").required(true))
        .get_matches();

    DEBUG.store(matches.get_flag("debug"), std::sync::atomic::Ordering::Relaxed);
    let binary_path = Path::new(matches.get_one::<String>("binary").map(String::as_str).expect("binary is required"));

    assert!(binary_path.exists(), "{} does not exist", binary_path.to_string_lossy());

    let generate_dir: Option<&Path> = matches.get_one::<String>("generate").map(String::as_str).map(Path::new).map(|p| {
        if p.exists() {
            p
        } else {
            panic!("{} does not exist", p.to_string_lossy());
        }
    });

    let id_mapping: Option<HashMap<String, u16>> = matches
        .get_one::<String>("id_mapping")
        .map(String::as_str)
        .map(fs::File::open)
        .transpose()
        .expect("Couldn't open id mapping file")
        .map(serde_json::from_reader)
        .transpose()
        .expect("Couldn't parse id mapping");

    let file_data = fs::read(binary_path).unwrap_or_else(|e| panic!("Couldn\'t read {}: {}", binary_path.to_string_lossy(), e));

    println!("[*] Extracting and parsing DDL from {binary_path:?}");
    let magic_bytes = &MAGIC.to_be_bytes()[..];
    let mut data = file_data.as_bytes();
    let mut namespaces = Vec::default();
    while !data.is_empty() {
        let pos = file_data.len() - data.len();
        if pos % 1000 == 0 {
            eprint!("\rPosition: {pos:#x}");
        }
        data = if data.starts_with(magic_bytes) {
            eprint!("\rPosition: {pos:#x}");
            eprintln!("\nParsing");
            let (d, mut namespace) = parse_ddl(data, pos).unwrap();
            if matches.get_flag("verbose") {
                println!("{namespace:#?}");
            }

            if id_mapping.is_some() {
                for el in &mut namespace.elements {
                    if let Element::ProtocolDeclaration(p) = el {
                        let id = id_mapping
                            .as_ref()
                            .and_then(|m| m.get(&format!("{}::{}", p.namespace, p.name1)).or_else(|| m.get(&p.name1)));
                        p.id = id.copied();
                    }
                }
            }

            namespaces.push(namespace);
            d
        } else {
            &data[1..]
        };
    }
    println!();

    if let Some(output) = matches.get_one::<String>("output").map(String::as_str) {
        println!("[*] Writing parsed DDL to {output}");
        serde_json::to_writer_pretty(fs::File::create(output).expect("could not open output file"), &namespaces).expect("error writing output");
    }

    if let Some(generate) = generate_dir {
        println!("[*] Generating source files in {generate:?}");
        let import_map = build_import_map(&namespaces);
        write_modules(
            generate,
            namespaces
                .iter()
                .map(|n| generate_source(generate, n, &import_map))
                .collect::<std::io::Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .fold(HashSet::new(), |mut s, v| {
                    s.extend(v);
                    s
                })
                .into_iter(),
            true,
        )
        .unwrap();
    }
}
