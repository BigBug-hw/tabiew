//! Acceptance tests compare the inspector with an independent Parquet reader.
use std::io::Cursor;

use polars::df;
use polars::prelude::*;
use polars_parquet::parquet::read::read_metadata;
use tabiew::binary::parquet::{RegionKind, parse};

fn multi_group_file() -> Vec<u8> {
    let mut frame = df! {
        "id" => (0..24i64).collect::<Vec<_>>(),
        "name" => (0..24).map(|i| if i % 3 == 0 { None } else { Some(format!("item-{i}")) }).collect::<Vec<_>>()
    }.unwrap();
    let mut bytes = Vec::new();
    ParquetWriter::new(&mut bytes)
        .with_row_group_size(Some(4))
        .finish(&mut frame)
        .unwrap();
    bytes
}

#[test]
fn multiple_row_groups_match_backend_column_ranges() {
    let bytes = multi_group_file();
    let metadata = read_metadata(&mut Cursor::new(&bytes)).unwrap();
    assert!(
        metadata.row_groups.len() > 1,
        "fixture must exercise multiple row groups"
    );
    let layout = parse(&bytes);
    assert!(layout.error.is_none(), "{:?}", layout.error);
    let actual: Vec<_> = layout
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::ColumnChunk)
        .map(|r| r.span.clone())
        .collect();
    let expected: Vec<_> = metadata
        .row_groups
        .iter()
        .flat_map(|rg| rg.parquet_columns().iter().map(|c| c.byte_range()))
        .collect();
    assert_eq!(actual, expected);
    let groups: Vec<_> = layout
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::RowGroup)
        .collect();
    assert_eq!(groups.len(), metadata.row_groups.len());
    for (group, expected_group) in groups.iter().zip(&metadata.row_groups) {
        let cols = expected_group.parquet_columns();
        let expected_start = cols.iter().map(|c| c.byte_range().start).min().unwrap();
        let expected_end = cols.iter().map(|c| c.byte_range().end).max().unwrap();
        assert_eq!(group.span, expected_start..expected_end);
    }
    for pair in groups.windows(2) {
        assert!(
            pair[0].span.end <= pair[1].span.start,
            "row groups must not overlap"
        );
    }
}

#[test]
fn nested_fixture_dictionary_pages_belong_to_their_columns() {
    let bytes = include_bytes!("fixtures/sample-nested.parquet");
    let layout = parse(bytes);
    assert!(layout.error.is_none(), "{:?}", layout.error);
    let chunks: Vec<_> = layout
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::ColumnChunk)
        .collect();
    let starts = [4, 119, 208, 302, 421, 552, 651];
    assert_eq!(
        chunks.iter().map(|c| c.span.start).collect::<Vec<_>>(),
        starts
    );
    assert_eq!(chunks[0].span, 4..119);
    for column in chunks {
        let pages: Vec<_> = layout
            .regions
            .iter()
            .filter(|r| {
                r.kind == RegionKind::PageHeader
                    && r.span.start >= column.span.start
                    && r.span.end <= column.span.end
            })
            .collect();
        assert!(!pages.is_empty());
        assert_eq!(pages[0].span.start, column.span.start);
        assert!(
            pages[0].label.contains("DICTIONARY_PAGE"),
            "{}",
            pages[0].label
        );
    }
    let labels = layout
        .tree
        .iter()
        .map(|n| n.label.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for name in [
        "path_in_schema",
        "total_compressed_size",
        "total_uncompressed_size",
        "dictionary_page_offset",
        "logicalType",
        "statistics",
        "encoding_stats",
        "repetition_type",
        "num_children",
    ] {
        assert!(labels.contains(name), "missing official field {name}");
    }
}

#[test]
fn truncation_and_footer_corruption_never_escape_file_bounds() {
    let bytes = multi_group_file();
    let mut cases: Vec<Vec<u8>> = (0..bytes.len())
        .step_by(37)
        .map(|end| bytes[..end].to_vec())
        .collect();
    for length in [0u32, 1, u32::MAX] {
        let mut damaged = bytes.clone();
        let footer = damaged.len() - 8;
        damaged[footer..footer + 4].copy_from_slice(&length.to_le_bytes());
        cases.push(damaged);
    }
    for damaged in cases {
        let outcome = std::panic::catch_unwind(|| parse(&damaged));
        assert!(
            outcome.is_ok(),
            "parser panicked on {} bytes",
            damaged.len()
        );
        let layout = outcome.unwrap();
        for region in layout.regions {
            assert!(region.span.start <= region.span.end);
            assert!(
                region.span.end <= damaged.len() as u64,
                "out-of-file region {:?}",
                region.span
            );
        }
    }
}

#[test]
fn every_nested_sample_page_decodes_with_dictionary_and_levels() {
    use tabiew::binary::parquet_decode::decode_page;
    let bytes = include_bytes!("fixtures/sample-nested.parquet");
    let layout = parse(bytes);
    let pages: Vec<_> = layout
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::PageHeader)
        .collect();
    assert_eq!(pages.len(), 14);
    let mut text = String::new();
    for page in pages {
        let details =
            decode_page(bytes, page.span.start).unwrap_or_else(|e| panic!("{}: {e}", page.label));
        assert_eq!(details.source_span.start, page.span.start);
        assert!(!details.decompressed.is_empty());
        assert!(
            !details.controls.iter().any(|c| c.contains("unavailable")),
            "{:?}",
            details.controls
        );
        text.push_str(&details.values.join("\n"));
        text.push('\n');
    }
    for expected in [
        "dictionary[0]",
        "dictionary_index=",
        "repetition_level=1",
        "definition_level=",
        "dictionary[0] = 1001",
        "dictionary[0] = \"Alice\"",
        "empty collection at",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
}

fn v2_file(compression: polars_parquet::write::CompressionOptions) -> Vec<u8> {
    use polars_parquet::write::{
        Encoding, FileWriter, RowGroupIterator, StatisticsOptions, Version, WriteOptions,
    };
    let frame = df! { "number" => [Some(41i64), None, Some(-7), Some(99)], "text" => [Some("alpha"), Some("beta"), None, Some("")] }.unwrap();
    let schema = frame.schema().to_arrow(CompatLevel::newest());
    let options = WriteOptions {
        statistics: StatisticsOptions::default(),
        version: Version::V2,
        compression,
        data_page_size: None,
    };
    let encodings = vec![vec![Encoding::Plain]; schema.len()].into();
    let chunks = frame.iter_chunks(CompatLevel::newest(), false).map(Ok);
    let groups = RowGroupIterator::try_new(chunks, &schema, options, encodings).unwrap();
    let mut writer = FileWriter::try_new(Vec::new(), schema, options).unwrap();
    for group in groups {
        writer.write(4, group.unwrap()).unwrap();
    }
    writer.end(None).unwrap();
    writer.into_inner()
}

#[test]
fn independently_written_v2_pages_restore_nulls_and_values_across_codecs() {
    use polars_parquet::write::CompressionOptions;
    use tabiew::binary::parquet_decode::decode_page;
    for codec in [
        CompressionOptions::Uncompressed,
        CompressionOptions::Snappy,
        CompressionOptions::Zstd(None),
        CompressionOptions::Gzip(None),
    ] {
        let bytes = v2_file(codec);
        let frame = ParquetReader::new(Cursor::new(&bytes)).finish().unwrap();
        assert_eq!(
            (0..4)
                .map(|i| frame.column("number").unwrap().i64().unwrap().get(i))
                .collect::<Vec<_>>(),
            [Some(41), None, Some(-7), Some(99)]
        );
        let layout = parse(&bytes);
        assert!(layout.error.is_none(), "{:?}", layout.error);
        let pages: Vec<_> = layout
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::PageHeader)
            .collect();
        assert_eq!(pages.len(), 2);
        let mut decoded = Vec::new();
        for page in pages {
            assert!(page.label.contains("DATA_PAGE_V2"));
            let detail = decode_page(&bytes, page.span.start).unwrap();
            assert!(
                !detail.controls.iter().any(|c| c.contains("unavailable")),
                "{:?}",
                detail.controls
            );
            assert!(
                detail
                    .controls
                    .iter()
                    .any(|c| c.contains("definition_levels:") && c.contains("byte_length="))
            );
            decoded.push(detail.values);
        }
        assert_eq!(decoded[0].len(), 4);
        assert!(decoded[0][0].ends_with(": 41"));
        assert!(decoded[0][1].ends_with(": null at number"));
        assert!(decoded[0][2].ends_with(": -7"));
        assert!(decoded[0][3].ends_with(": 99"));
        assert_eq!(decoded[1].len(), 4);
        assert!(decoded[1][0].ends_with(": \"alpha\""));
        assert!(decoded[1][1].ends_with(": \"beta\""));
        assert!(decoded[1][2].ends_with(": null at text"));
        assert!(decoded[1][3].ends_with(": \"\""));
    }
}
