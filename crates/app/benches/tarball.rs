use std::io::Write as _;

use mix_app::bootstrap::tarball::{sha256_hex, unpack};

fn main() {
    divan::main();
}

const KIB: usize = 1024;

fn payload(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut bytes = Vec::with_capacity(len + 8);
    while bytes.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

fn xz_tar(files: usize, size: usize) -> Vec<u8> {
    let contents = payload(size);
    let mut builder = tar::Builder::new(Vec::new());
    for n in 0..files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, format!("store/file-{n}"), &contents[..])
            .expect("writing a tar entry in memory never fails");
    }
    let archive = builder.into_inner().expect("finishing the tar archive");

    let mut writer = liblzma::write::XzEncoder::new(Vec::new(), 6);
    writer.write_all(&archive).expect("compressing with xz");
    writer.finish().expect("finishing the xz stream")
}

#[divan::bench(args = [64 * KIB, 1024 * KIB, 4096 * KIB])]
fn sha256_digest(bencher: divan::Bencher, len: usize) {
    let bytes = payload(len);
    bencher.bench(|| sha256_hex(divan::black_box(&bytes)));
}

#[divan::bench]
fn unpack_many_small_files(bencher: divan::Bencher) {
    let archive = xz_tar(64, 2 * KIB);
    bencher
        .with_inputs(|| tempfile::tempdir().expect("creating a temporary directory"))
        .bench_local_values(|dest| unpack(divan::black_box(&archive), dest.path()).unwrap());
}

#[divan::bench]
fn unpack_one_large_file(bencher: divan::Bencher) {
    let archive = xz_tar(1, 512 * KIB);
    bencher
        .with_inputs(|| tempfile::tempdir().expect("creating a temporary directory"))
        .bench_local_values(|dest| unpack(divan::black_box(&archive), dest.path()).unwrap());
}
