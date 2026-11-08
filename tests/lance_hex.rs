//! Independent file-level regression cases for the Lance inspector.
use tabiew::binary::lance::{RegionKind, parse};

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
    out
}
fn scalar(id: u64, n: u64) -> Vec<u8> {
    let mut b = varint(id << 3);
    b.extend(varint(n));
    b
}
fn blob(id: u64, payload: &[u8]) -> Vec<u8> {
    let mut b = varint(id << 3 | 2);
    b.extend(varint(payload.len() as u64));
    b.extend(payload);
    b
}
fn file(global: &[u8], column: &[u8]) -> Vec<u8> {
    let mut b = global.to_vec();
    let col = b.len() as u64;
    b.extend(column);
    let cmo = b.len() as u64;
    b.extend(col.to_le_bytes());
    b.extend((column.len() as u64).to_le_bytes());
    let gbo = b.len() as u64;
    b.extend(0u64.to_le_bytes());
    b.extend((global.len() as u64).to_le_bytes());
    b.extend(col.to_le_bytes());
    b.extend(cmo.to_le_bytes());
    b.extend(gbo.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    b.extend(0u16.to_le_bytes());
    b.extend(3u16.to_le_bytes());
    b.extend(b"LANC");
    b
}
fn text(bytes: &[u8]) -> String {
    let l = parse(bytes);
    assert!(l.is_lance, "{:?}", l.error);
    assert!(l.error.is_none(), "{:?}", l.error);
    for n in &l.tree {
        assert!(
            n.span.start <= n.span.end && n.span.end <= bytes.len() as u64,
            "{n:?}"
        );
    }
    l.tree
        .iter()
        .map(|n| n.label.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn fixtures_preserve_real_names_and_buffer_spans() {
    let inode = include_bytes!("fixtures/inode.lance");
    let t = text(inode);
    assert!(t.contains("mtime"), "{t}");
    assert!(!t.contains("\"?\""), "{t}");
    let dentry = include_bytes!("fixtures/dentry.lance");
    let t = text(dentry);
    for name in [
        "parent_ino",
        "buffer_offsets",
        "buffer_sizes",
        "logical_type",
        "bits_per_value",
    ] {
        assert!(t.contains(name), "missing {name}: {t}");
    }
    let l = parse(dentry);
    let spans: Vec<_> = l
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::DataBuffer)
        .map(|r| r.span.clone())
        .collect();
    for expected in [0..8, 64..72, 128..132, 192..200] {
        assert!(spans.contains(&expected), "{spans:?}");
    }
    assert!(!l.references.is_empty());
    assert!(
        l.references.iter().any(|r| {
            r.target == (128..132)
                && dentry[r.source.start as usize..r.source.end as usize] == [0x80, 0x01]
        }),
        "packed offset must reference its own buffer with exact varint span"
    );
}

#[test]
fn metadata_is_not_a_field_and_blob_inner_is_not_zone_index() {
    let field = [blob(2, b"mtime"), blob(5, b"uint64")].concat();
    let entry = [blob(1, b"bar"), blob(2, b"baz")].concat();
    let schema = [blob(1, &field), blob(5, &entry)].concat();
    let descriptor = [blob(1, &schema), scalar(2, 1)].concat();
    // ColumnEncoding.blob.inner.values is an empty message.
    let values = blob(1, &[]);
    let column_encoding = blob(3, &blob(1, &values));
    let any = [
        blob(1, b"/lance.encodings.ColumnEncoding"),
        blob(2, &column_encoding),
    ]
    .concat();
    let column = blob(1, &blob(2, &blob(1, &any)));
    let t = text(&file(&descriptor, &column));
    assert!(
        t.contains("metadata") && t.contains("bar") && t.contains("62 61 7a"),
        "{t}"
    );
    assert!(
        t.contains("blob") && t.contains("inner") && t.contains("values"),
        "{t}"
    );
    assert!(!t.contains("rows_per_zone"), "{t}");
}

#[test]
fn unknown_type_does_not_acquire_known_encoding_names() {
    let any = [
        blob(1, b"/custom.FutureEncoding"),
        blob(2, &blob(1, &scalar(1, 64))),
    ]
    .concat();
    let column = blob(1, &blob(2, &blob(1, &any)));
    let t = text(&file(&[], &column));
    assert!(t.contains("custom.FutureEncoding"), "{t}");
    assert!(!t.contains("Flat") && !t.contains("bits_per_value"), "{t}");
}

#[test]
fn corrupted_footer_and_table_values_never_panic_or_escape_file() {
    let original = include_bytes!("fixtures/dentry.lance");
    let footer = original.len() - 40;
    for offset in [footer, footer + 8, footer + 16, 691, 699, 739, 747] {
        for value in [0, 1, original.len() as u64, u64::MAX, u64::MAX - 1] {
            let mut b = original.to_vec();
            b[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            let l = parse(&b);
            for r in l.regions {
                assert!(
                    r.span.start <= r.span.end && r.span.end <= b.len() as u64,
                    "{r:?}"
                );
            }
        }
    }
    for n in 0..original.len() {
        let _ = parse(&original[..n]);
    }
}

#[test]
fn declared_new_encoding_types_use_their_own_field_names() {
    for (url, payload, expected) in [
        (
            "/lance.encodings21.PageLayout",
            blob(2, &[]),
            "constant_layout",
        ),
        (
            "/lance.encodings23.PlanarLayout",
            blob(1, &blob(1, &[])),
            "dense",
        ),
    ] {
        let any = [blob(1, url.as_bytes()), blob(2, &payload)].concat();
        let page = blob(4, &blob(2, &blob(1, &any)));
        let column = blob(2, &page);
        let t = text(&file(&[], &column));
        assert!(t.contains(expected), "{t}");
        assert!(
            !t.contains("Nullable") && !t.contains("FixedSizeList"),
            "{t}"
        );
    }
}

#[test]
fn repeated_buffers_keep_all_packed_segments_and_unpacked_entries() {
    let zeros = vec![0; 1100];
    let page = [blob(1, &zeros), scalar(1, 0), blob(2, &zeros), scalar(2, 0)].concat();
    let bytes = file(&[], &blob(2, &page));
    let l = parse(&bytes);
    assert!(l.error.is_none(), "{:?}", l.error);
    // One reference for each offset and size entry, including entries after 1024.
    assert!(
        l.references.len() >= 2202,
        "only {} references",
        l.references.len()
    );
}
