//! Official message/field descriptors from the supplied Lance protobuf definitions.
pub fn field(ty: &str, number: u32) -> Option<(&'static str, &'static str, bool)> {
    Some(match (ty, number) {
        ("lance.file.FileDescriptor", 1) => ("schema", "lance.file.Schema", false),
        ("lance.file.FileDescriptor", 2) => ("length", "uint64", false),
        ("lance.file.Schema", 1) => ("fields", "lance.file.Field", true),
        ("lance.file.Schema", 5) => ("metadata", "map<string,bytes>", false),
        ("lance.file.Metadata", 1) => ("manifest_position", "uint64", false),
        ("lance.file.Metadata", 2) => ("batch_offsets", "int32", true),
        ("lance.file.Metadata", 3) => ("page_table_position", "uint64", false),
        ("lance.file.Metadata.StatisticsMetadata", 1) => ("schema", "lance.file.Field", true),
        ("lance.file.Metadata.StatisticsMetadata", 2) => ("fields", "int32", true),
        ("lance.file.Metadata.StatisticsMetadata", 3) => ("page_table_position", "uint64", false),
        ("lance.file.Metadata", 5) => (
            "statistics",
            "lance.file.Metadata.StatisticsMetadata",
            false,
        ),
        ("lance.file.Dictionary", 1) => ("offset", "int64", false),
        ("lance.file.Dictionary", 2) => ("length", "int64", false),
        ("lance.file.Field", 1) => ("type", "lance.file.Field.Type", false),
        ("lance.file.Field", 2) => ("name", "string", false),
        ("lance.file.Field", 3) => ("id", "int32", false),
        ("lance.file.Field", 4) => ("parent_id", "int32", false),
        ("lance.file.Field", 5) => ("logical_type", "string", false),
        ("lance.file.Field", 6) => ("nullable", "bool", false),
        ("lance.file.Field", 10) => ("metadata", "map<string,bytes>", false),
        ("lance.file.Field", 12) => ("unenforced_primary_key", "bool", false),
        ("lance.file.Field", 13) => ("unenforced_primary_key_position", "uint32", false),
        ("lance.file.Field", 14) => ("unenforced_clustering_key", "bool", false),
        ("lance.file.Field", 15) => ("unenforced_clustering_key_position", "uint32", false),
        ("lance.file.Field", 7) => ("encoding", "lance.file.Encoding", false),
        ("lance.file.Field", 8) => ("dictionary", "lance.file.Dictionary", false),
        ("lance.file.Field", 9) => ("extension_name", "string", false),
        ("lance.file.v2.DeferredEncoding", 1) => ("buffer_location", "uint64", false),
        ("lance.file.v2.DeferredEncoding", 2) => ("buffer_length", "uint64", false),
        ("lance.file.v2.DirectEncoding", 1) => ("encoding", "bytes", false),
        ("lance.file.v2.Encoding", 1) => ("indirect", "lance.file.v2.DeferredEncoding", false),
        ("lance.file.v2.Encoding", 2) => ("direct", "lance.file.v2.DirectEncoding", false),
        ("lance.file.v2.Encoding", 3) => ("none", "google.protobuf.Empty", false),
        ("lance.file.v2.ColumnMetadata.Page", 1) => ("buffer_offsets", "uint64", true),
        ("lance.file.v2.ColumnMetadata.Page", 2) => ("buffer_sizes", "uint64", true),
        ("lance.file.v2.ColumnMetadata.Page", 3) => ("length", "uint64", false),
        ("lance.file.v2.ColumnMetadata.Page", 4) => ("encoding", "lance.file.v2.Encoding", false),
        ("lance.file.v2.ColumnMetadata.Page", 5) => ("priority", "uint64", false),
        ("lance.file.v2.ColumnMetadata", 1) => ("encoding", "lance.file.v2.Encoding", false),
        ("lance.file.v2.ColumnMetadata", 2) => ("pages", "lance.file.v2.ColumnMetadata.Page", true),
        ("lance.file.v2.ColumnMetadata", 3) => ("buffer_offsets", "uint64", true),
        ("lance.file.v2.ColumnMetadata", 4) => ("buffer_sizes", "uint64", true),
        ("lance.encodings.Buffer", 1) => ("buffer_index", "uint32", false),
        ("lance.encodings.Buffer", 2) => {
            ("buffer_type", "lance.encodings.Buffer.BufferType", false)
        }
        ("lance.encodings.Nullable.NoNull", 1) => {
            ("values", "lance.encodings.ArrayEncoding", false)
        }
        ("lance.encodings.Nullable.SomeNull", 1) => {
            ("validity", "lance.encodings.ArrayEncoding", false)
        }
        ("lance.encodings.Nullable.SomeNull", 2) => {
            ("values", "lance.encodings.ArrayEncoding", false)
        }
        ("lance.encodings.Nullable", 1) => ("no_nulls", "lance.encodings.Nullable.NoNull", false),
        ("lance.encodings.Nullable", 2) => {
            ("some_nulls", "lance.encodings.Nullable.SomeNull", false)
        }
        ("lance.encodings.Nullable", 3) => ("all_nulls", "lance.encodings.Nullable.AllNull", false),
        ("lance.encodings.List", 1) => ("offsets", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.List", 2) => ("null_offset_adjustment", "uint64", false),
        ("lance.encodings.List", 3) => ("num_items", "uint64", false),
        ("lance.encodings.FixedSizeList", 1) => ("dimension", "uint32", false),
        ("lance.encodings.FixedSizeList", 3) => ("has_validity", "bool", false),
        ("lance.encodings.FixedSizeList", 2) => ("items", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Compression", 1) => ("scheme", "string", false),
        ("lance.encodings.Compression", 2) => ("level", "int32", false),
        ("lance.encodings.Flat", 1) => ("bits_per_value", "uint64", false),
        ("lance.encodings.Flat", 2) => ("buffer", "lance.encodings.Buffer", false),
        ("lance.encodings.Flat", 3) => ("compression", "lance.encodings.Compression", false),
        ("lance.encodings.Constant", 1) => ("value", "bytes", false),
        ("lance.encodings.Bitpacked", 1) => ("compressed_bits_per_value", "uint64", false),
        ("lance.encodings.Bitpacked", 2) => ("uncompressed_bits_per_value", "uint64", false),
        ("lance.encodings.Bitpacked", 3) => ("buffer", "lance.encodings.Buffer", false),
        ("lance.encodings.Bitpacked", 4) => ("signed", "bool", false),
        ("lance.encodings.BitpackedForNonNeg", 1) => ("compressed_bits_per_value", "uint64", false),
        ("lance.encodings.BitpackedForNonNeg", 2) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings.BitpackedForNonNeg", 3) => ("buffer", "lance.encodings.Buffer", false),
        ("lance.encodings.InlineBitpacking", 2) => ("uncompressed_bits_per_value", "uint64", false),
        ("lance.encodings.OutOfLineBitpacking", 2) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings.OutOfLineBitpacking", 3) => {
            ("compressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings.Binary", 1) => ("indices", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Binary", 2) => ("bytes", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Binary", 3) => ("null_adjustment", "uint64", false),
        ("lance.encodings.Variable", 1) => ("bits_per_offset", "uint32", false),
        ("lance.encodings.Fsst", 1) => ("binary", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Fsst", 2) => ("symbol_table", "bytes", false),
        ("lance.encodings.Dictionary", 1) => ("indices", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Dictionary", 2) => ("items", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.Dictionary", 3) => ("num_dictionary_items", "uint32", false),
        ("lance.encodings.PackedStruct", 1) => ("inner", "lance.encodings.ArrayEncoding", true),
        ("lance.encodings.PackedStruct", 2) => ("buffer", "lance.encodings.Buffer", false),
        ("lance.encodings.PackedStructFixedWidthMiniBlock", 1) => {
            ("Flat", "lance.encodings.ArrayEncoding", false)
        }
        ("lance.encodings.PackedStructFixedWidthMiniBlock", 2) => {
            ("bits_per_values", "uint32", true)
        }
        ("lance.encodings.FixedSizeBinary", 1) => ("bytes", "lance.encodings.ArrayEncoding", false),
        ("lance.encodings.FixedSizeBinary", 2) => ("byte_width", "uint32", false),
        ("lance.encodings.Block", 1) => ("scheme", "string", false),
        ("lance.encodings.Rle", 1) => ("bits_per_value", "uint64", false),
        ("lance.encodings.ByteStreamSplit", 1) => ("bits_per_value", "uint64", false),
        ("lance.encodings.GeneralMiniBlock", 1) => {
            ("inner", "lance.encodings.ArrayEncoding", false)
        }
        ("lance.encodings.GeneralMiniBlock", 2) => {
            ("compression", "lance.encodings.Compression", false)
        }
        ("lance.encodings.ArrayEncoding", 1) => ("flat", "lance.encodings.Flat", false),
        ("lance.encodings.ArrayEncoding", 2) => ("nullable", "lance.encodings.Nullable", false),
        ("lance.encodings.ArrayEncoding", 3) => {
            ("fixed_size_list", "lance.encodings.FixedSizeList", false)
        }
        ("lance.encodings.ArrayEncoding", 4) => ("list", "lance.encodings.List", false),
        ("lance.encodings.ArrayEncoding", 5) => ("struct", "lance.encodings.SimpleStruct", false),
        ("lance.encodings.ArrayEncoding", 6) => ("binary", "lance.encodings.Binary", false),
        ("lance.encodings.ArrayEncoding", 7) => ("dictionary", "lance.encodings.Dictionary", false),
        ("lance.encodings.ArrayEncoding", 8) => ("fsst", "lance.encodings.Fsst", false),
        ("lance.encodings.ArrayEncoding", 9) => {
            ("packed_struct", "lance.encodings.PackedStruct", false)
        }
        ("lance.encodings.ArrayEncoding", 10) => ("bitpacked", "lance.encodings.Bitpacked", false),
        ("lance.encodings.ArrayEncoding", 11) => (
            "fixed_size_binary",
            "lance.encodings.FixedSizeBinary",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 12) => (
            "bitpacked_for_non_neg",
            "lance.encodings.BitpackedForNonNeg",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 13) => ("constant", "lance.encodings.Constant", false),
        ("lance.encodings.ArrayEncoding", 14) => (
            "inline_bitpacking",
            "lance.encodings.InlineBitpacking",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 15) => (
            "out_of_line_bitpacking",
            "lance.encodings.OutOfLineBitpacking",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 16) => ("variable", "lance.encodings.Variable", false),
        ("lance.encodings.ArrayEncoding", 17) => (
            "packed_struct_fixed_width_mini_block",
            "lance.encodings.PackedStructFixedWidthMiniBlock",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 18) => ("block", "lance.encodings.Block", false),
        ("lance.encodings.ArrayEncoding", 19) => ("rle", "lance.encodings.Rle", false),
        ("lance.encodings.ArrayEncoding", 20) => (
            "general_mini_block",
            "lance.encodings.GeneralMiniBlock",
            false,
        ),
        ("lance.encodings.ArrayEncoding", 21) => (
            "byte_stream_split",
            "lance.encodings.ByteStreamSplit",
            false,
        ),
        ("lance.encodings.ZoneIndex", 1) => ("rows_per_zone", "uint32", false),
        ("lance.encodings.ZoneIndex", 2) => ("zone_map_buffer", "lance.encodings.Buffer", false),
        ("lance.encodings.ZoneIndex", 3) => ("inner", "lance.encodings.ColumnEncoding", false),
        ("lance.encodings.Blob", 1) => ("inner", "lance.encodings.ColumnEncoding", false),
        ("lance.encodings.ColumnEncoding", 1) => ("values", "google.protobuf.Empty", false),
        ("lance.encodings.ColumnEncoding", 2) => ("zone_index", "lance.encodings.ZoneIndex", false),
        ("lance.encodings.ColumnEncoding", 3) => ("blob", "lance.encodings.Blob", false),
        ("lance.encodings21.MiniBlockLayout", 1) => (
            "rep_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.MiniBlockLayout", 2) => (
            "def_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.MiniBlockLayout", 3) => (
            "value_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.MiniBlockLayout", 4) => {
            ("dictionary", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.MiniBlockLayout", 5) => ("num_dictionary_items", "uint64", false),
        ("lance.encodings21.MiniBlockLayout", 6) => {
            ("layers", "lance.encodings21.RepDefLayer", true)
        }
        ("lance.encodings21.MiniBlockLayout", 7) => ("num_buffers", "uint64", false),
        ("lance.encodings21.MiniBlockLayout", 8) => ("repetition_index_depth", "uint32", false),
        ("lance.encodings21.MiniBlockLayout", 9) => ("num_items", "uint64", false),
        ("lance.encodings21.MiniBlockLayout", 10) => ("has_large_chunk", "bool", false),
        ("lance.encodings21.FullZipLayout", 1) => ("bits_rep", "uint32", false),
        ("lance.encodings21.FullZipLayout", 2) => ("bits_def", "uint32", false),
        ("lance.encodings21.FullZipLayout", 3) => ("bits_per_value", "uint32", false),
        ("lance.encodings21.FullZipLayout", 4) => ("bits_per_offset", "uint32", false),
        ("lance.encodings21.FullZipLayout", 5) => ("num_items", "uint32", false),
        ("lance.encodings21.FullZipLayout", 6) => ("num_visible_items", "uint32", false),
        ("lance.encodings21.FullZipLayout", 7) => (
            "value_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.FullZipLayout", 8) => ("layers", "lance.encodings21.RepDefLayer", true),
        ("lance.encodings21.ConstantLayout", 5) => {
            ("layers", "lance.encodings21.RepDefLayer", true)
        }
        ("lance.encodings21.ConstantLayout", 6) => ("inline_value", "bytes", false),
        ("lance.encodings21.ConstantLayout", 7) => (
            "rep_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.ConstantLayout", 8) => (
            "def_compression",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.ConstantLayout", 9) => ("num_rep_values", "uint64", false),
        ("lance.encodings21.ConstantLayout", 10) => ("num_def_values", "uint64", false),
        ("lance.encodings21.BlobLayout", 1) => {
            ("inner_layout", "lance.encodings21.PageLayout", false)
        }
        ("lance.encodings21.BlobLayout", 2) => ("layers", "lance.encodings21.RepDefLayer", true),
        ("lance.encodings21.PageLayout", 1) => (
            "mini_block_layout",
            "lance.encodings21.MiniBlockLayout",
            false,
        ),
        ("lance.encodings21.PageLayout", 2) => {
            ("constant_layout", "lance.encodings21.ConstantLayout", false)
        }
        ("lance.encodings21.PageLayout", 3) => {
            ("full_zip_layout", "lance.encodings21.FullZipLayout", false)
        }
        ("lance.encodings21.PageLayout", 4) => {
            ("blob_layout", "lance.encodings21.BlobLayout", false)
        }
        ("lance.encodings21.BufferCompression", 1) => {
            ("scheme", "lance.encodings21.CompressionScheme", false)
        }
        ("lance.encodings21.BufferCompression", 2) => ("level", "int32", false),
        ("lance.encodings21.Flat", 1) => ("bits_per_value", "uint64", false),
        ("lance.encodings21.Flat", 2) => ("data", "lance.encodings21.BufferCompression", false),
        ("lance.encodings21.Variable", 1) => {
            ("offsets", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.Variable", 2) => {
            ("values", "lance.encodings21.BufferCompression", false)
        }
        ("lance.encodings21.Constant", 1) => ("value", "bytes", false),
        ("lance.encodings21.OutOfLineBitpacking", 1) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings21.OutOfLineBitpacking", 3) => {
            ("values", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.InlineBitpacking", 1) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings21.InlineBitpacking", 2) => {
            ("values", "lance.encodings21.BufferCompression", false)
        }
        ("lance.encodings21.Fsst", 1) => ("symbol_table", "bytes", false),
        ("lance.encodings21.Fsst", 2) => ("values", "lance.encodings21.CompressiveEncoding", false),
        ("lance.encodings21.Dictionary", 1) => {
            ("indices", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.Dictionary", 2) => {
            ("items", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.Dictionary", 3) => ("num_dictionary_items", "uint32", false),
        ("lance.encodings21.Rle", 1) => ("values", "lance.encodings21.CompressiveEncoding", false),
        ("lance.encodings21.Rle", 2) => (
            "run_lengths",
            "lance.encodings21.CompressiveEncoding",
            false,
        ),
        ("lance.encodings21.FixedSizeList", 1) => ("items_per_value", "uint64", false),
        ("lance.encodings21.FixedSizeList", 3) => ("has_validity", "bool", false),
        ("lance.encodings21.FixedSizeList", 2) => {
            ("values", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.PackedStruct", 1) => ("bits_per_value", "uint64", true),
        ("lance.encodings21.PackedStruct", 2) => {
            ("values", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.VariablePackedStruct", 1) => (
            "fields",
            "lance.encodings21.VariablePackedStruct.FieldEncoding",
            true,
        ),
        ("lance.encodings21.VariablePackedStruct.FieldEncoding", 1) => {
            ("value", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.VariablePackedStruct.FieldEncoding", 2) => {
            ("bits_per_value", "uint64", false)
        }
        ("lance.encodings21.VariablePackedStruct.FieldEncoding", 3) => {
            ("bits_per_length", "uint64", false)
        }
        ("lance.encodings21.General", 1) => {
            ("compression", "lance.encodings21.BufferCompression", false)
        }
        ("lance.encodings21.General", 3) => {
            ("values", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.ByteStreamSplit", 1) => {
            ("values", "lance.encodings21.CompressiveEncoding", false)
        }
        ("lance.encodings21.CompressiveEncoding", 1) => ("flat", "lance.encodings21.Flat", false),
        ("lance.encodings21.CompressiveEncoding", 2) => {
            ("variable", "lance.encodings21.Variable", false)
        }
        ("lance.encodings21.CompressiveEncoding", 3) => {
            ("constant", "lance.encodings21.Constant", false)
        }
        ("lance.encodings21.CompressiveEncoding", 4) => (
            "out_of_line_bitpacking",
            "lance.encodings21.OutOfLineBitpacking",
            false,
        ),
        ("lance.encodings21.CompressiveEncoding", 5) => (
            "inline_bitpacking",
            "lance.encodings21.InlineBitpacking",
            false,
        ),
        ("lance.encodings21.CompressiveEncoding", 6) => ("fsst", "lance.encodings21.Fsst", false),
        ("lance.encodings21.CompressiveEncoding", 7) => {
            ("dictionary", "lance.encodings21.Dictionary", false)
        }
        ("lance.encodings21.CompressiveEncoding", 8) => ("rle", "lance.encodings21.Rle", false),
        ("lance.encodings21.CompressiveEncoding", 9) => (
            "byte_stream_split",
            "lance.encodings21.ByteStreamSplit",
            false,
        ),
        ("lance.encodings21.CompressiveEncoding", 10) => {
            ("general", "lance.encodings21.General", false)
        }
        ("lance.encodings21.CompressiveEncoding", 11) => {
            ("fixed_size_list", "lance.encodings21.FixedSizeList", false)
        }
        ("lance.encodings21.CompressiveEncoding", 12) => {
            ("packed_struct", "lance.encodings21.PackedStruct", false)
        }
        ("lance.encodings21.CompressiveEncoding", 13) => (
            "variable_packed_struct",
            "lance.encodings21.VariablePackedStruct",
            false,
        ),
        ("lance.encodings23.PlanarLayout", 1) => {
            ("structure", "lance.encodings23.StructuralPlane", false)
        }
        ("lance.encodings23.PlanarLayout", 2) => ("values", "lance.encodings23.ValuePlane", false),
        ("lance.encodings23.StructuralPlane", 1) => {
            ("dense", "lance.encodings23.DenseStructure", false)
        }
        ("lance.encodings23.StructuralPlane", 2) => {
            ("sparse", "lance.encodings23.SparseStructure", false)
        }
        ("lance.encodings23.DenseStructure", 1) => {
            ("layers", "lance.encodings23.RepDefLayer", true)
        }
        ("lance.encodings23.SparseStructure", 1) => {
            ("layers", "lance.encodings23.SparseStructuralLayer", true)
        }
        ("lance.encodings23.ValuePlane", 1) => {
            ("chunked", "lance.encodings23.ChunkedValues", false)
        }
        ("lance.encodings23.ValuePlane", 2) => ("zipped", "lance.encodings23.ZippedValues", false),
        ("lance.encodings23.ValuePlane", 3) => {
            ("constant", "lance.encodings23.ConstantValue", false)
        }
        ("lance.encodings23.ValuePlane", 4) => ("none", "lance.encodings23.NoValues", false),
        ("lance.encodings23.ValuePlane", 5) => {
            ("indexed", "lance.encodings23.IndexedValues", false)
        }
        ("lance.encodings23.IndexedValues", 1) => {
            ("indices", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.IndexedValues", 2) => {
            ("items", "lance.encodings23.ZippedValues", false)
        }
        ("lance.encodings23.ChunkedValues", 1) => (
            "compression",
            "lance.encodings23.CompressiveEncoding",
            false,
        ),
        ("lance.encodings23.ChunkedValues", 2) => {
            ("levels", "lance.encodings23.DenseChunkLevels", false)
        }
        ("lance.encodings23.ChunkedValues", 3) => {
            ("dictionary", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.ChunkedValues", 4) => ("num_dictionary_items", "uint64", false),
        ("lance.encodings23.ChunkedValues", 5) => ("num_buffers", "uint64", false),
        ("lance.encodings23.ChunkedValues", 6) => ("num_items", "uint64", false),
        ("lance.encodings23.DenseChunkLevels", 1) => {
            ("repetition", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.DenseChunkLevels", 2) => {
            ("definition", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.DenseChunkLevels", 3) => ("repetition_index_depth", "uint32", false),
        ("lance.encodings23.ZippedValues", 1) => (
            "compression",
            "lance.encodings23.CompressiveEncoding",
            false,
        ),
        ("lance.encodings23.ZippedValues", 2) => {
            ("levels", "lance.encodings23.DenseZipLevels", false)
        }
        ("lance.encodings23.ZippedValues", 3) => ("bits_per_value", "uint32", false),
        ("lance.encodings23.ZippedValues", 4) => ("bits_per_offset", "uint32", false),
        ("lance.encodings23.ZippedValues", 5) => ("num_items", "uint64", false),
        ("lance.encodings23.ZippedValues", 6) => ("num_visible_items", "uint64", false),
        ("lance.encodings23.DenseZipLevels", 1) => ("bits_repetition", "uint32", false),
        ("lance.encodings23.DenseZipLevels", 2) => ("bits_definition", "uint32", false),
        ("lance.encodings23.ConstantValue", 1) => ("scalar", "bytes", false),
        ("lance.encodings23.SparseStructuralLayer", 1) => {
            ("validity", "lance.encodings23.SparseValiditySet", false)
        }
        ("lance.encodings23.SparseStructuralLayer", 2) => {
            ("list", "lance.encodings23.SparseListLayer", false)
        }
        ("lance.encodings23.SparseStructuralLayer", 3) => (
            "fixed_size_list",
            "lance.encodings23.SparseFixedSizeListLayer",
            false,
        ),
        ("lance.encodings23.SparseListLayer", 1) => ("num_child_slots", "uint64", false),
        ("lance.encodings23.SparseListLayer", 2) => (
            "non_empty_positions",
            "lance.encodings23.CompressiveEncoding",
            false,
        ),
        ("lance.encodings23.SparseListLayer", 3) => ("num_non_empty_positions", "uint64", false),
        ("lance.encodings23.SparseListLayer", 4) => {
            ("counts", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.SparseListLayer", 5) => {
            ("validity", "lance.encodings23.SparseValiditySet", false)
        }
        ("lance.encodings23.SparseFixedSizeListLayer", 1) => ("dimension", "uint64", false),
        ("lance.encodings23.SparseFixedSizeListLayer", 2) => {
            ("validity", "lance.encodings23.SparseValiditySet", false)
        }
        ("lance.encodings23.SparseValiditySet", 1) => (
            "meaning",
            "lance.encodings23.SparseValiditySet.Meaning",
            false,
        ),
        ("lance.encodings23.SparseValiditySet", 2) => {
            ("positions", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.SparseValiditySet", 3) => ("num_positions", "uint64", false),
        ("lance.encodings23.BufferCompression", 1) => {
            ("scheme", "lance.encodings23.CompressionScheme", false)
        }
        ("lance.encodings23.BufferCompression", 2) => ("level", "int32", false),
        ("lance.encodings23.Flat", 1) => ("bits_per_value", "uint64", false),
        ("lance.encodings23.Flat", 2) => ("data", "lance.encodings23.BufferCompression", false),
        ("lance.encodings23.Variable", 1) => {
            ("offsets", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.Variable", 2) => {
            ("values", "lance.encodings23.BufferCompression", false)
        }
        ("lance.encodings23.Constant", 1) => ("value", "bytes", false),
        ("lance.encodings23.Range", 1) => ("uncompressed_bits_per_value", "uint64", false),
        ("lance.encodings23.Range", 2) => ("start", "uint64", false),
        ("lance.encodings23.Range", 3) => ("step", "uint64", false),
        ("lance.encodings23.Delta", 1) => ("uncompressed_bits_per_value", "uint64", false),
        ("lance.encodings23.Delta", 2) => ("base", "uint64", false),
        ("lance.encodings23.Delta", 3) => {
            ("deltas", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.OutOfLineBitpacking", 1) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings23.OutOfLineBitpacking", 3) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.InlineBitpacking", 1) => {
            ("uncompressed_bits_per_value", "uint64", false)
        }
        ("lance.encodings23.InlineBitpacking", 2) => {
            ("values", "lance.encodings23.BufferCompression", false)
        }
        ("lance.encodings23.Fsst", 1) => ("symbol_table", "bytes", false),
        ("lance.encodings23.Fsst", 2) => ("values", "lance.encodings23.CompressiveEncoding", false),
        ("lance.encodings23.Dictionary", 1) => {
            ("indices", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.Dictionary", 2) => {
            ("items", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.Dictionary", 3) => ("num_dictionary_items", "uint32", false),
        ("lance.encodings23.Rle", 1) => ("values", "lance.encodings23.CompressiveEncoding", false),
        ("lance.encodings23.Rle", 2) => (
            "run_lengths",
            "lance.encodings23.CompressiveEncoding",
            false,
        ),
        ("lance.encodings23.FixedSizeList", 1) => ("items_per_value", "uint64", false),
        ("lance.encodings23.FixedSizeList", 3) => ("has_validity", "bool", false),
        ("lance.encodings23.FixedSizeList", 2) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.PackedStruct", 1) => ("bits_per_value", "uint64", true),
        ("lance.encodings23.PackedStruct", 2) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.VariablePackedStruct", 1) => (
            "fields",
            "lance.encodings23.VariablePackedStruct.FieldEncoding",
            true,
        ),
        ("lance.encodings23.VariablePackedStruct.FieldEncoding", 1) => {
            ("value", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.VariablePackedStruct.FieldEncoding", 2) => {
            ("bits_per_value", "uint64", false)
        }
        ("lance.encodings23.VariablePackedStruct.FieldEncoding", 3) => {
            ("bits_per_length", "uint64", false)
        }
        ("lance.encodings23.General", 1) => {
            ("compression", "lance.encodings23.BufferCompression", false)
        }
        ("lance.encodings23.General", 3) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.ByteStreamSplit", 1) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.Alp", 1) => ("bits_per_value", "uint32", false),
        ("lance.encodings23.AxisDelta", 1) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.AxisDelta", 2) => ("axis_length", "uint64", false),
        ("lance.encodings23.AxisDelta", 3) => ("inner_stride", "uint64", false),
        ("lance.encodings23.Pcodec", 1) => {
            ("values", "lance.encodings23.CompressiveEncoding", false)
        }
        ("lance.encodings23.CompressiveEncoding", 1) => ("flat", "lance.encodings23.Flat", false),
        ("lance.encodings23.CompressiveEncoding", 2) => {
            ("variable", "lance.encodings23.Variable", false)
        }
        ("lance.encodings23.CompressiveEncoding", 3) => {
            ("constant", "lance.encodings23.Constant", false)
        }
        ("lance.encodings23.CompressiveEncoding", 4) => (
            "out_of_line_bitpacking",
            "lance.encodings23.OutOfLineBitpacking",
            false,
        ),
        ("lance.encodings23.CompressiveEncoding", 5) => (
            "inline_bitpacking",
            "lance.encodings23.InlineBitpacking",
            false,
        ),
        ("lance.encodings23.CompressiveEncoding", 6) => ("fsst", "lance.encodings23.Fsst", false),
        ("lance.encodings23.CompressiveEncoding", 7) => {
            ("dictionary", "lance.encodings23.Dictionary", false)
        }
        ("lance.encodings23.CompressiveEncoding", 8) => ("rle", "lance.encodings23.Rle", false),
        ("lance.encodings23.CompressiveEncoding", 9) => (
            "byte_stream_split",
            "lance.encodings23.ByteStreamSplit",
            false,
        ),
        ("lance.encodings23.CompressiveEncoding", 10) => {
            ("general", "lance.encodings23.General", false)
        }
        ("lance.encodings23.CompressiveEncoding", 11) => {
            ("fixed_size_list", "lance.encodings23.FixedSizeList", false)
        }
        ("lance.encodings23.CompressiveEncoding", 12) => {
            ("packed_struct", "lance.encodings23.PackedStruct", false)
        }
        ("lance.encodings23.CompressiveEncoding", 13) => (
            "variable_packed_struct",
            "lance.encodings23.VariablePackedStruct",
            false,
        ),
        ("lance.encodings23.CompressiveEncoding", 14) => {
            ("range", "lance.encodings23.Range", false)
        }
        ("lance.encodings23.CompressiveEncoding", 15) => {
            ("delta", "lance.encodings23.Delta", false)
        }
        ("lance.encodings23.CompressiveEncoding", 16) => {
            ("axis_delta", "lance.encodings23.AxisDelta", false)
        }
        ("lance.encodings23.CompressiveEncoding", 17) => ("alp", "lance.encodings23.Alp", false),
        ("lance.encodings23.CompressiveEncoding", 18) => {
            ("pcodec", "lance.encodings23.Pcodec", false)
        }
        _ => return None,
    })
}
pub fn enum_value(ty: &str, value: u64) -> Option<&'static str> {
    Some(match (ty, value) {
        ("lance.file.Encoding", 0) => "NONE",
        ("lance.file.Encoding", 1) => "PLAIN",
        ("lance.file.Encoding", 2) => "VAR_BINARY",
        ("lance.file.Encoding", 3) => "DICTIONARY",
        ("lance.file.Encoding", 4) => "RLE",
        ("lance.file.Field.Type", 0) => "PARENT",
        ("lance.file.Field.Type", 1) => "REPEATED",
        ("lance.file.Field.Type", 2) => "LEAF",
        ("lance.encodings.Buffer.BufferType", 0) => "page",
        ("lance.encodings.Buffer.BufferType", 1) => "column",
        ("lance.encodings.Buffer.BufferType", 2) => "file",
        ("lance.encodings21.RepDefLayer", 0) => "REPDEF_UNSPECIFIED",
        ("lance.encodings21.RepDefLayer", 1) => "REPDEF_ALL_VALID_ITEM",
        ("lance.encodings21.RepDefLayer", 2) => "REPDEF_ALL_VALID_LIST",
        ("lance.encodings21.RepDefLayer", 3) => "REPDEF_NULLABLE_ITEM",
        ("lance.encodings21.RepDefLayer", 4) => "REPDEF_NULLABLE_LIST",
        ("lance.encodings21.RepDefLayer", 5) => "REPDEF_EMPTYABLE_LIST",
        ("lance.encodings21.RepDefLayer", 6) => "REPDEF_NULL_AND_EMPTY_LIST",
        ("lance.encodings21.CompressionScheme", 0) => "COMPRESSION_ALGORITHM_UNSPECIFIED",
        ("lance.encodings21.CompressionScheme", 1) => "COMPRESSION_ALGORITHM_LZ4",
        ("lance.encodings21.CompressionScheme", 2) => "COMPRESSION_ALGORITHM_ZSTD",
        ("lance.encodings23.SparseValiditySet.Meaning", 0) => "SPARSE_VALIDITY_UNSPECIFIED",
        ("lance.encodings23.SparseValiditySet.Meaning", 1) => "SPARSE_VALIDITY_NULL_POSITIONS",
        ("lance.encodings23.SparseValiditySet.Meaning", 2) => "SPARSE_VALIDITY_VALID_POSITIONS",
        ("lance.encodings23.RepDefLayer", 0) => "REPDEF_UNSPECIFIED",
        ("lance.encodings23.RepDefLayer", 1) => "REPDEF_ALL_VALID_ITEM",
        ("lance.encodings23.RepDefLayer", 2) => "REPDEF_ALL_VALID_LIST",
        ("lance.encodings23.RepDefLayer", 3) => "REPDEF_NULLABLE_ITEM",
        ("lance.encodings23.RepDefLayer", 4) => "REPDEF_NULLABLE_LIST",
        ("lance.encodings23.RepDefLayer", 5) => "REPDEF_EMPTYABLE_LIST",
        ("lance.encodings23.RepDefLayer", 6) => "REPDEF_NULL_AND_EMPTY_LIST",
        ("lance.encodings23.CompressionScheme", 0) => "COMPRESSION_ALGORITHM_UNSPECIFIED",
        ("lance.encodings23.CompressionScheme", 1) => "COMPRESSION_ALGORITHM_LZ4",
        ("lance.encodings23.CompressionScheme", 2) => "COMPRESSION_ALGORITHM_ZSTD",
        _ => return None,
    })
}
pub fn is_message(ty: &str) -> bool {
    matches!(
        ty,
        "lance.encodings.ArrayEncoding"
            | "lance.encodings.Binary"
            | "lance.encodings.Bitpacked"
            | "lance.encodings.BitpackedForNonNeg"
            | "lance.encodings.Blob"
            | "lance.encodings.Block"
            | "lance.encodings.Buffer"
            | "lance.encodings.ByteStreamSplit"
            | "lance.encodings.ColumnEncoding"
            | "lance.encodings.Compression"
            | "lance.encodings.Constant"
            | "lance.encodings.Dictionary"
            | "lance.encodings.FixedSizeBinary"
            | "lance.encodings.FixedSizeList"
            | "lance.encodings.Flat"
            | "lance.encodings.Fsst"
            | "lance.encodings.GeneralMiniBlock"
            | "lance.encodings.InlineBitpacking"
            | "lance.encodings.List"
            | "lance.encodings.Nullable"
            | "lance.encodings.Nullable.AllNull"
            | "lance.encodings.Nullable.NoNull"
            | "lance.encodings.Nullable.SomeNull"
            | "lance.encodings.OutOfLineBitpacking"
            | "lance.encodings.PackedStruct"
            | "lance.encodings.PackedStructFixedWidthMiniBlock"
            | "lance.encodings.Rle"
            | "lance.encodings.SimpleStruct"
            | "lance.encodings.Variable"
            | "lance.encodings.ZoneIndex"
            | "lance.encodings21.BlobLayout"
            | "lance.encodings21.BufferCompression"
            | "lance.encodings21.ByteStreamSplit"
            | "lance.encodings21.CompressiveEncoding"
            | "lance.encodings21.Constant"
            | "lance.encodings21.ConstantLayout"
            | "lance.encodings21.Dictionary"
            | "lance.encodings21.FixedSizeList"
            | "lance.encodings21.Flat"
            | "lance.encodings21.Fsst"
            | "lance.encodings21.FullZipLayout"
            | "lance.encodings21.General"
            | "lance.encodings21.InlineBitpacking"
            | "lance.encodings21.MiniBlockLayout"
            | "lance.encodings21.OutOfLineBitpacking"
            | "lance.encodings21.PackedStruct"
            | "lance.encodings21.PageLayout"
            | "lance.encodings21.Rle"
            | "lance.encodings21.Variable"
            | "lance.encodings21.VariablePackedStruct"
            | "lance.encodings21.VariablePackedStruct.FieldEncoding"
            | "lance.encodings23.Alp"
            | "lance.encodings23.AxisDelta"
            | "lance.encodings23.BufferCompression"
            | "lance.encodings23.ByteStreamSplit"
            | "lance.encodings23.ChunkedValues"
            | "lance.encodings23.CompressiveEncoding"
            | "lance.encodings23.Constant"
            | "lance.encodings23.ConstantValue"
            | "lance.encodings23.Delta"
            | "lance.encodings23.DenseChunkLevels"
            | "lance.encodings23.DenseStructure"
            | "lance.encodings23.DenseZipLevels"
            | "lance.encodings23.Dictionary"
            | "lance.encodings23.FixedSizeList"
            | "lance.encodings23.Flat"
            | "lance.encodings23.Fsst"
            | "lance.encodings23.General"
            | "lance.encodings23.IndexedValues"
            | "lance.encodings23.InlineBitpacking"
            | "lance.encodings23.NoValues"
            | "lance.encodings23.OutOfLineBitpacking"
            | "lance.encodings23.PackedStruct"
            | "lance.encodings23.Pcodec"
            | "lance.encodings23.PlanarLayout"
            | "lance.encodings23.Range"
            | "lance.encodings23.Rle"
            | "lance.encodings23.SparseFixedSizeListLayer"
            | "lance.encodings23.SparseListLayer"
            | "lance.encodings23.SparseStructuralLayer"
            | "lance.encodings23.SparseStructure"
            | "lance.encodings23.SparseValiditySet"
            | "lance.encodings23.StructuralPlane"
            | "lance.encodings23.ValuePlane"
            | "lance.encodings23.Variable"
            | "lance.encodings23.VariablePackedStruct"
            | "lance.encodings23.VariablePackedStruct.FieldEncoding"
            | "lance.encodings23.ZippedValues"
            | "lance.file.Dictionary"
            | "lance.file.Field"
            | "lance.file.FileDescriptor"
            | "lance.file.Metadata"
            | "lance.file.Metadata.StatisticsMetadata"
            | "lance.file.Schema"
            | "lance.file.v2.ColumnMetadata"
            | "lance.file.v2.ColumnMetadata.Page"
            | "lance.file.v2.DeferredEncoding"
            | "lance.file.v2.DirectEncoding"
            | "lance.file.v2.Encoding"
    )
}
