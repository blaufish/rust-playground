use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::sync::mpsc::sync_channel;
use std::time::Instant;
use std::thread;
use std::io;

use chrono::prelude::*;
use clap::Parser;
use sha2::{Sha256, Digest};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    input: std::path::PathBuf,

    #[arg(short, long)]
    output: std::path::PathBuf,

    #[arg(long, default_value_t = 10)]
    queue_size: usize,

    #[arg(long, default_value = "1M")]
    block_size: String,
}

struct Configuration {
    queue_size : usize,
    block_size : usize,
    read_bytes : usize,
    read_files : usize,
    debug : bool,
    last_update: Instant,
}

impl Configuration {
    fn emit_debug_message(&mut self) -> bool {
        if ! self.debug {
            return false;
        }
        let update = self.last_update.elapsed().as_secs() > 5 ;
        if update {
            self.last_update = Instant::now();
        }
        return update;
    }

    fn debug_message(&self) -> String {
        let suf: Vec<&str> = vec!["", "K", "M", "G", "T", "P" ];
        let mut size : usize = self.read_bytes;
        let mut vec : Vec<usize> = Vec::new();
        if size == 0 {
            return "".to_string();
        }
        else {
            while size > 0 {
                let reminder = size % 1024;
                size = size / 1024;
                vec.push(reminder);
            }
        }
        let mut result : String = "\r".to_string();
        let mut max = 2;
        for i in (0..vec.len()).rev() {
            let reminder = vec[i];
            if reminder == 0 {
                continue;
            }
            let mut s = "?";
            if i < suf.len() {
                s = suf[i];
            }
            let tmp : String = format!("{}{} ", reminder, s);
            result = result + &tmp;
            max = max - 1;
            if max == 0 {
                break;
            }
        }
        let tmp : String = format!("{} files      ", self.read_files);
        result = result + &tmp;

        return result;
    }
}

enum Message {
    Block(Vec<u8>),
    Done,
    Error,
}

enum StatusMessage {
    StatusIncBlock(usize),
    StatusDone
}

fn copy(cfg: &mut Configuration, input: std::path::PathBuf, output: std::path::PathBuf) -> Result<String, io::Error> {
    let block_size : usize = cfg.block_size;
    let queue_size : usize = cfg.queue_size;

    let mut fi = File::open(input)?;
    let mut fo = File::create(output)?;

    let (read_tx, read_rx) = sync_channel::<Message>(queue_size);
    let (sha_tx, sha_rx) = sync_channel::<Message>(queue_size);
    let (file_write_tx, file_write_rx) = sync_channel::<Message>(queue_size);
    let (status_tx, status_rx) = sync_channel::<StatusMessage>(queue_size);

    let read_thread = thread::spawn(move || {
        let mut failed = true;
        let mut heap_buf: Vec<u8> = Vec::with_capacity(block_size);
        heap_buf.resize(block_size, 0x00);
        loop {
            match fi.read(&mut heap_buf[0..block_size]) {
                Ok(0) => {
                    failed = false;
                    break;
                }
                Ok(n) => {
                    if let Err(e) = read_tx.send(Message::Block(heap_buf[0..n].to_vec())) {
                        eprintln!("Error: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }
        if failed {
            if let Err(e) = read_tx.send(Message::Error) {
                eprintln!("Error: {}", e);
            }
            return ;
        }
        if let Err(e) = read_tx.send(Message::Done) {
            eprintln!("Error: {}", e);
        }
    });

    let router_thread = thread::spawn( move || {
        let mut err = false;
        loop {
            match read_rx.recv() {
                Ok(Message::Block(block)) => {
                    if let Err(e) = sha_tx.send(Message::Block(block.clone())) {
                        eprintln!("Error: {}", e);
                        err = true;
                    }
                    if let Err(e) = file_write_tx.send(Message::Block(block.clone())) {
                        eprintln!("Error: {}", e);
                        err = true;
                    }
                    if let Err(e) = status_tx.send(StatusMessage::StatusIncBlock(block.len())) {
                        eprintln!("Error: {}", e);
                        err = true;
                    }
                    if err {
                        break;
                    }
                }
                Ok(Message::Done) => {
                    break;
                }
                Ok(Message::Error) => {
                    err = true;
                    break;
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }
        if err {
            if let Err(e) = sha_tx.send(Message::Error) {
                eprintln!("Error: {}", e);
            }
            if let Err(e) = file_write_tx.send(Message::Error) {
                eprintln!("Error: {}", e);
            }
        }
        else {
            if let Err(e) = sha_tx.send(Message::Done) {
                eprintln!("Error: {}", e);
            }
            if let Err(e) = file_write_tx.send(Message::Done) {
                eprintln!("Error: {}", e);
            }
        }
        if let Err(e) = status_tx.send(StatusMessage::StatusDone) {
            eprintln!("Error: {}", e);
        }
    });

    let sha_thread = thread::spawn( move || -> Result<String,()> {
        let mut h1 = Sha256::new();
        let mut incomplete = true;
        loop {
            match sha_rx.recv() {
                Ok(Message::Block(block)) => {
                    h1.update(&block);
                }
                Ok(Message::Error) => {
                    break;
                }
                Ok(Message::Done) => {
                    incomplete = false;
                    break;
                }
                Err(e) => {
                    eprintln!("Error T-SHA: {}", e);
                    break;
                }
            }
        }
        if incomplete {
            return Err(());
        }
        let digest = h1.finalize();
        let strdigest = format!("{:x}", digest);
        return Ok(strdigest);
    });

    let file_write_thread = thread::spawn( move || {
        loop {
            match file_write_rx.recv() {
                Ok(Message::Block(block)) => {
                    if let Err(e) = fo.write_all(&block) {
                        eprintln!("Error T-FW: {}", e);
                        break;
                    }
                }
                Ok(Message::Error) => {
                    break;
                }
                Ok(Message::Done) => {
                    break;
                }
                Err(e) => {
                    eprintln!("Error T-FW: {}", e);
                    break;
                }
            }
        }
    });

    let mut stderr = io::stderr();
    //let debug_msg : Option<String> = None;
    loop {
        match status_rx.recv() {
            Ok(StatusMessage::StatusDone) => {
                break;
            },
            Ok(StatusMessage::StatusIncBlock(u)) => {
                cfg.read_bytes = cfg.read_bytes + u;

                if cfg.emit_debug_message() {
                    let debug_msg = cfg.debug_message();
                    let _ = stderr.write(debug_msg.as_bytes());
                    let _ = stderr.flush();
                }
            },
            Err( e ) => {
                eprintln!("Error status loop: {}", e);
            }
        }
    }

    let mut failed = true;
    let mut result : String = "".to_string();

    if let Err(_) = read_thread.join() {
        panic!("Failure to join read thread");
    }
    if let Err(_) = router_thread.join() {
        panic!("Failure to join router thread");
    }
    if let Err(_) = file_write_thread.join() {
        panic!("Failure to join file write thread");
    }

    let sha_result: Result<String,()>;
    match sha_thread.join() {
        Ok(s) => {
            sha_result = s;
        }
        Err(_) => panic!("Failure to join sha thread"),
    }
    match sha_result {
        Ok(s) => {
            if s.len() == 64 {
                result = s;
                failed = false;
            }
            else {
                eprintln!("Bad SHA-256 received: '{}'", s);
            }
        }
        Err(_) => {
            eprintln!("SHA-thread completed errornously!");
        }
    }

    if failed {
        return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
    }

    cfg.read_files = cfg.read_files + 1;

    let debug_msg = cfg.debug_message();
    let _ = stderr.write(debug_msg.as_bytes());
    let _ = stderr.flush();

    Ok(result)
}


fn copy_directory(
    cfg: &mut Configuration,
    input: std::path::PathBuf,
    output: std::path::PathBuf) -> io::Result<()> {
    let rel = std::path::PathBuf::new();

    let now = Local::now();
    let date_string = now.format("shasum.%Y-%m-%d.%H.%M.%S.txt").to_string();
    let mut foptions = OpenOptions::new();
    let _ = foptions.write(true);
    let _ = foptions.create_new(true);

    let mut path_shasum = output.clone();
    path_shasum.push(date_string);

    let mut shasum_file;
    match foptions.open(&path_shasum) {
        Ok(file) => {
            shasum_file = file;
        },
        Err(e) => {
            return Err(e);
        }
    }
    println!("Writing SHA256 sums to: {}", path_shasum.display());
    return copy_dir(cfg, &mut shasum_file, input, rel, output);
}

fn copy_dir(
    cfg: &mut Configuration,
    shasum_file: &mut std::fs::File,
    input: std::path::PathBuf,
    rel:  std::path::PathBuf,
    output: std::path::PathBuf) -> io::Result<()> {
    for entry in fs::read_dir(input)? {
        let entry = entry?;
        let path = entry.path();
        let mut output_path = output.clone();
        let mut rel2 = rel.clone();
        match path.file_name() {
            Some(s) => {
                output_path.push(s);
                rel2.push(s);
            },
            None => continue, //TODO error handling
        }
        if path.is_dir() {
            //println!("dir: {} --> {}", path.display(), output_path.display());
            if !output_path.exists() {
                fs::create_dir(output_path.clone())?;
            }
            copy_dir(cfg, shasum_file, path, rel2, output_path)?;
        }
        else if path.is_file() {
            //println!("file: {}", path.display());
            //println!("file out: {}", output_path.clone().display());
            match copy(cfg, path, output_path) {
                Ok(s) => {
                    let string = format!("{}  {}\n", s.to_lowercase(), rel2.display());
                    let _ = shasum_file.write_all( string.as_bytes() );
                },
                Err(_s) => {
                    return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                }
            }
        }
    }
    Ok(())
}

fn s2i(string : String) -> usize {
    let mut prefix : usize = 0;
    let mut exponent : usize = 1;
    for c in string.chars() {
        match c {
            'K' => exponent = 1024,
            'M' => exponent = 1024 * 1024,
            'G' => exponent = 1024 * 1024 * 1024,
            '0' => prefix = prefix * 10,
            '1' => prefix = prefix * 10 + 1,
            '2' => prefix = prefix * 10 + 2,
            '3' => prefix = prefix * 10 + 3,
            '4' => prefix = prefix * 10 + 4,
            '5' => prefix = prefix * 10 + 5,
            '6' => prefix = prefix * 10 + 6,
            '7' => prefix = prefix * 10 + 7,
            '8' => prefix = prefix * 10 + 8,
            '9' => prefix = prefix * 10 + 9,
            _ => eprintln!("Unable to parse: {}", string)
        }
    }
    let result = prefix * exponent;
    if result < 1 {
       eprintln!("Unable to parse: {}", string)
    }
    return result;
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let queue_size = args.queue_size;
    let block_size = s2i(args.block_size);
    let mut cfg = Configuration {
        queue_size: queue_size,
        block_size: block_size,
        read_bytes: 0,
        read_files: 0,
        debug: false,
        last_update: Instant::now(),
    };

    if ! args.input.is_dir() {
        eprintln!("Directory {} is not a directory", args.input.display());
        return Ok(())
    }

    if ! args.output.is_dir() {
        eprintln!("Directory {} is not a directory", args.output.display());
        return Ok(())
    }
    println!("Block size: {}", block_size);
    println!("Queue size: {}", queue_size);

    let stderr = io::stderr();
    cfg.debug = stderr.is_terminal();

    if let Err(e) = copy_directory(&mut cfg, args.input, args.output) {
        eprintln!("copy_dir failed: {}", e);
        return Err(e);
    }

    Ok(())
}
