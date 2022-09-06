use std::ops::{Range, RangeInclusive};

use fastfield_codecs::ip_codec::IntervallDecompressor;

use crate::fastfield::{
    DynamicFastFieldReader, FastFieldReader, FastFieldReaderCodecWrapperU128, FastValue,
    FastValueU128, MultiValueLength,
};
use crate::DocId;

/// Reader for a multivalued `u64` fast field.
///
/// The reader is implemented as two `u64` fast field.
///
/// The `vals_reader` will access the concatenated list of all
/// values for all reader.
/// The `idx_reader` associated, for each document, the index of its first value.
#[derive(Clone)]
pub struct MultiValuedFastFieldReader<Item: FastValue> {
    idx_reader: DynamicFastFieldReader<u64>,
    vals_reader: DynamicFastFieldReader<Item>,
}

impl<Item: FastValue> MultiValuedFastFieldReader<Item> {
    pub(crate) fn open(
        idx_reader: DynamicFastFieldReader<u64>,
        vals_reader: DynamicFastFieldReader<Item>,
    ) -> MultiValuedFastFieldReader<Item> {
        MultiValuedFastFieldReader {
            idx_reader,
            vals_reader,
        }
    }

    /// Returns `[start, end)`, such that the values associated
    /// to the given document are `start..end`.
    #[inline]
    fn range(&self, doc: DocId) -> Range<u64> {
        let start = self.idx_reader.get(doc);
        let end = self.idx_reader.get(doc + 1);
        start..end
    }

    /// Returns the array of values associated to the given `doc`.
    #[inline]
    fn get_vals_for_range(&self, range: Range<u64>, vals: &mut Vec<Item>) {
        let len = (range.end - range.start) as usize;
        vals.resize(len, Item::make_zero());
        self.vals_reader.get_range(range.start, &mut vals[..]);
    }

    /// Returns the array of values associated to the given `doc`.
    #[inline]
    pub fn get_vals(&self, doc: DocId, vals: &mut Vec<Item>) {
        let range = self.range(doc);
        self.get_vals_for_range(range, vals);
    }

    /// Returns the minimum value for this fast field.
    ///
    /// The min value does not take in account of possible
    /// deleted document, and should be considered as a lower bound
    /// of the actual mimimum value.
    pub fn min_value(&self) -> Item {
        self.vals_reader.min_value()
    }

    /// Returns the maximum value for this fast field.
    ///
    /// The max value does not take in account of possible
    /// deleted document, and should be considered as an upper bound
    /// of the actual maximum value.
    pub fn max_value(&self) -> Item {
        self.vals_reader.max_value()
    }

    /// Returns the number of values associated with the document `DocId`.
    #[inline]
    pub fn num_vals(&self, doc: DocId) -> usize {
        let range = self.range(doc);
        (range.end - range.start) as usize
    }

    /// Returns the overall number of values in this field  .
    #[inline]
    pub fn total_num_vals(&self) -> u64 {
        self.idx_reader.max_value()
    }
}

impl<Item: FastValue> MultiValueLength for MultiValuedFastFieldReader<Item> {
    fn get_range(&self, doc_id: DocId) -> std::ops::Range<u64> {
        self.range(doc_id)
    }
    fn get_len(&self, doc_id: DocId) -> u64 {
        self.num_vals(doc_id) as u64
    }
    fn get_total_len(&self) -> u64 {
        self.total_num_vals() as u64
    }
}

/// Reader for a multivalued `u128` fast field.
///
/// The reader is implemented as a `u64` fast field for the index and a `u128` fast field.
///
/// The `vals_reader` will access the concatenated list of all
/// values for all reader.
/// The `idx_reader` associated, for each document, the index of its first value.
#[derive(Clone)]
pub struct MultiValuedU128FastFieldReader<Item: FastValueU128> {
    idx_reader: DynamicFastFieldReader<u64>,
    vals_reader: FastFieldReaderCodecWrapperU128<Item, IntervallDecompressor>,
}

impl<Item: FastValueU128> MultiValuedU128FastFieldReader<Item> {
    pub(crate) fn open(
        idx_reader: DynamicFastFieldReader<u64>,
        vals_reader: FastFieldReaderCodecWrapperU128<Item, IntervallDecompressor>,
    ) -> MultiValuedU128FastFieldReader<Item> {
        Self {
            idx_reader,
            vals_reader,
        }
    }

    /// Returns `[start, end)`, such that the values associated
    /// to the given document are `start..end`.
    #[inline]
    fn range(&self, doc: DocId) -> Range<u64> {
        let start = self.idx_reader.get(doc);
        let end = self.idx_reader.get(doc + 1);
        start..end
    }

    /// Returns the array of values associated to the given `doc`.
    #[inline]
    pub fn get_first_val(&self, doc: DocId) -> Option<Item> {
        let range = self.range(doc);
        if range.is_empty() {
            return None;
        }
        self.vals_reader.get_val(range.start)
    }

    /// Returns the array of values associated to the given `doc`.
    #[inline]
    fn get_vals_for_range(&self, range: Range<u64>, vals: &mut Vec<Item>) {
        let len = (range.end - range.start) as usize;
        vals.resize(len, Item::make_zero());
        self.vals_reader.get_range(range.start, &mut vals[..]);
    }

    /// Returns the array of values associated to the given `doc`.
    #[inline]
    pub fn get_vals(&self, doc: DocId, vals: &mut Vec<Item>) {
        let range = self.range(doc);
        self.get_vals_for_range(range, vals);
    }

    /// Returns all docids which are in the provided value range
    pub fn get_between_vals(&self, range: RangeInclusive<Item>) -> Vec<DocId> {
        let positions = self.vals_reader.get_between_vals(range);

        positions_to_docids(&positions, self)
    }

    /// Iterates over all elements in the fast field
    pub fn iter(&self) -> impl Iterator<Item = Option<Item>> + '_ {
        self.vals_reader.iter()
    }

    /// Returns the minimum value for this fast field.
    ///
    /// The min value does not take in account of possible
    /// deleted document, and should be considered as a lower bound
    /// of the actual mimimum value.
    pub fn min_value(&self) -> Item {
        self.vals_reader.min_value()
    }

    /// Returns the maximum value for this fast field.
    ///
    /// The max value does not take in account of possible
    /// deleted document, and should be considered as an upper bound
    /// of the actual maximum value.
    pub fn max_value(&self) -> Item {
        self.vals_reader.max_value()
    }

    /// Returns the number of values associated with the document `DocId`.
    #[inline]
    pub fn num_vals(&self, doc: DocId) -> usize {
        let range = self.range(doc);
        (range.end - range.start) as usize
    }

    /// Returns the overall number of values in this field  .
    #[inline]
    pub fn total_num_vals(&self) -> u64 {
        self.idx_reader.max_value()
    }
}

/// Converts a list of positions of values in a 1:n index to the corresponding list of DocIds.
///
/// Since there is no index for value pos -> docid, but docid -> value pos range, we scan the index.
///
/// Correctness: positions needs to be sorted.
///
/// TODO: Instead of a linear scan we can employ a binary search to match a docid to its value
/// position.
fn positions_to_docids<T: MultiValueLength>(positions: &[usize], multival_idx: &T) -> Vec<DocId> {
    let mut docs = vec![];
    let mut cur_doc = 0u32;
    let mut last_doc = None;

    for pos in positions {
        loop {
            let range = multival_idx.get_range(cur_doc);
            if range.contains(&(*pos as u64)) {
                // avoid duplicates
                if Some(cur_doc) == last_doc {
                    break;
                }
                docs.push(cur_doc);
                last_doc = Some(cur_doc);
                break;
            }
            cur_doc += 1;
        }
    }

    docs
}

impl<Item: FastValueU128> MultiValueLength for MultiValuedU128FastFieldReader<Item> {
    fn get_range(&self, doc_id: DocId) -> std::ops::Range<u64> {
        self.range(doc_id)
    }
    fn get_len(&self, doc_id: DocId) -> u64 {
        self.num_vals(doc_id) as u64
    }

    fn get_total_len(&self) -> u64 {
        self.total_num_vals() as u64
    }
}

#[cfg(test)]
mod tests {

    use crate::core::Index;
    use crate::schema::{Cardinality, Facet, FacetOptions, NumericOptions, Schema};

    #[test]
    fn test_multifastfield_reader() -> crate::Result<()> {
        let mut schema_builder = Schema::builder();
        let facet_field = schema_builder.add_facet_field("facets", FacetOptions::default());
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut index_writer = index.writer_for_tests()?;
        index_writer.add_document(doc!(
            facet_field => Facet::from("/category/cat2"),
            facet_field => Facet::from("/category/cat1"),
        ))?;
        index_writer.add_document(doc!(facet_field => Facet::from("/category/cat2")))?;
        index_writer.add_document(doc!(facet_field => Facet::from("/category/cat3")))?;
        index_writer.commit()?;
        let searcher = index.reader()?.searcher();
        let segment_reader = searcher.segment_reader(0);
        let mut facet_reader = segment_reader.facet_reader(facet_field)?;

        let mut facet = Facet::root();
        {
            facet_reader.facet_from_ord(1, &mut facet).unwrap();
            assert_eq!(facet, Facet::from("/category"));
        }
        {
            facet_reader.facet_from_ord(2, &mut facet).unwrap();
            assert_eq!(facet, Facet::from("/category/cat1"));
        }
        {
            facet_reader.facet_from_ord(3, &mut facet).unwrap();
            assert_eq!(format!("{}", facet), "/category/cat2");
            assert_eq!(facet, Facet::from("/category/cat2"));
        }
        {
            facet_reader.facet_from_ord(4, &mut facet).unwrap();
            assert_eq!(facet, Facet::from("/category/cat3"));
        }

        let mut vals = Vec::new();
        {
            facet_reader.facet_ords(0, &mut vals);
            assert_eq!(&vals[..], &[2, 3]);
        }
        {
            facet_reader.facet_ords(1, &mut vals);
            assert_eq!(&vals[..], &[3]);
        }
        {
            facet_reader.facet_ords(2, &mut vals);
            assert_eq!(&vals[..], &[4]);
        }
        Ok(())
    }

    #[test]
    fn test_multifastfield_reader_min_max() -> crate::Result<()> {
        let mut schema_builder = Schema::builder();
        let field_options = NumericOptions::default()
            .set_indexed()
            .set_fast(Cardinality::MultiValues);
        let item_field = schema_builder.add_i64_field("items", field_options);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut index_writer = index
            .writer_for_tests()
            .expect("Failed to create index writer.");
        index_writer.add_document(doc!(
            item_field => 2i64,
            item_field => 3i64,
            item_field => -2i64,
        ))?;
        index_writer.add_document(doc!(item_field => 6i64, item_field => 3i64))?;
        index_writer.add_document(doc!(item_field => 4i64))?;
        index_writer.commit()?;
        let searcher = index.reader()?.searcher();
        let segment_reader = searcher.segment_reader(0);
        let field_reader = segment_reader.fast_fields().i64s(item_field)?;

        assert_eq!(field_reader.min_value(), -2);
        assert_eq!(field_reader.max_value(), 6);
        Ok(())
    }
}
