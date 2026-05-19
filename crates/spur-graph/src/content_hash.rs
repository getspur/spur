use sha1::{Digest, Sha1};

pub fn git_blob_oid(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn compute_graph_content_hash<I, P, O>(entries: I) -> String
where
    I: IntoIterator<Item = (P, O)>,
    P: AsRef<str>,
    O: AsRef<str>,
{
    let mut entries: Vec<(String, String)> = entries
        .into_iter()
        .map(|(path, oid)| (path.as_ref().to_string(), oid.as_ref().to_string()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut hasher = blake3::Hasher::new();
    for (path, oid) in entries {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(oid.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::{compute_graph_content_hash, git_blob_oid};

    #[test]
    fn git_blob_oid_matches_known_vectors() {
        assert_eq!(
            git_blob_oid(b"hello world"),
            "95d09f2b10159347eece71399a7e2e907ea3df4f"
        );
        assert_eq!(
            git_blob_oid(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            git_blob_oid(&[0x00, 0x01, 0xff, 0xfe]),
            "ad2f38543fc2bba3468a77f36137c23378420463"
        );
    }

    #[test]
    fn git_blob_oid_matches_git_hash_object_for_generated_inputs() {
        let mut seed = 0x5eed_cafe_d15e_a5e5_u64;
        for len in 0..50 {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                bytes.push((seed & 0xff) as u8);
            }

            let mut child = Command::new("git")
                .args(["hash-object", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn git hash-object");
            child
                .stdin
                .as_mut()
                .expect("git stdin")
                .write_all(&bytes)
                .expect("write git stdin");
            let output = child.wait_with_output().expect("wait for git hash-object");
            assert!(
                output.status.success(),
                "git hash-object failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = String::from_utf8(output.stdout).expect("git hash-object UTF-8");
            assert_eq!(git_blob_oid(&bytes), expected.trim_end());
        }
    }

    #[test]
    fn graph_content_hash_is_sort_stable() {
        let first = compute_graph_content_hash([
            ("src/b.rs", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            ("src/a.rs", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ]);
        let second = compute_graph_content_hash([
            ("src/a.rs", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ("src/b.rs", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ]);

        assert_eq!(first, second);
    }

    #[test]
    fn graph_content_hash_includes_path_boundaries() {
        let left = compute_graph_content_hash([("ab", "c"), ("d", "ef")]);
        let right = compute_graph_content_hash([("a", "bc"), ("d", "ef")]);

        assert_ne!(left, right);
    }
}
