//! A small authentication plugin for the tests. It asks the person for a word,
//! and answers every server prompt with the word's reverse.

use std::io::{Read, Write};

fn read_message() -> Option<(u8, Vec<u8>)> {
    let mut stdin = std::io::stdin().lock();
    let mut len = [0u8; 4];
    stdin.read_exact(&mut len).ok()?;
    let mut msg = vec![0u8; u32::from_be_bytes(len) as usize];
    stdin.read_exact(&mut msg).ok()?;
    Some((msg[0], msg[1..].to_vec()))
}

fn send(kind: u8, body: &[u8]) {
    let mut out = std::io::stdout().lock();
    out.write_all(&((body.len() + 1) as u32).to_be_bytes()).unwrap();
    out.write_all(&[kind]).unwrap();
    out.write_all(body).unwrap();
    out.flush().unwrap();
}

fn string(s: &[u8]) -> Vec<u8> {
    let mut b = (s.len() as u32).to_be_bytes().to_vec();
    b.extend(s);
    b
}

fn main() {
    while let Some((kind, body)) = read_message() {
        match kind {
            1 => {
                let mut r = 2u32.to_be_bytes().to_vec();
                r.extend(string(b""));
                send(2, &r);
            }
            3 => send(4, &[]),
            20 => {
                let mut r = [string(b"Plugin"), string(b""), string(b"")].concat();
                r.extend(1u32.to_be_bytes());
                r.extend(string(b"Plugin word: "));
                r.push(1);
                send(22, &r);
            }
            23 => {
                let n = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                let word: Vec<u8> = body[8..8 + n].iter().rev().copied().collect();
                let mut r = 1u32.to_be_bytes().to_vec();
                r.extend(string(&word));
                send(21, &r);
            }
            _ => {}
        }
    }
}
