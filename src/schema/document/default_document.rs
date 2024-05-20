use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::Ipv6Addr;

use columnar::MonotonicallyMappableToU128;
use common::{
    read_u32_vint, read_u32_vint_no_advance, serialize_vint_u32, write_u32_vint,
    BinarySerializable, DateTime,
};
use serde_json::Map;
pub use CompactDoc as TantivyDocument;

use super::{ReferenceValue, ReferenceValueLeaf, Value};
use crate::schema::document::{
    DeserializeError, Document, DocumentDeserialize, DocumentDeserializer,
};
use crate::schema::field_type::ValueParsingError;
use crate::schema::{Facet, Field, NamedFieldDocument, OwnedValue, Schema};
use crate::tokenizer::PreTokenizedString;

#[repr(packed)]
#[derive(Debug, Clone)]
/// A field value pair in the compact tantivy document
pub struct FieldValueAddr {
    pub field: u16,
    pub value: ValueAddr,
}

#[derive(Debug, Clone)]
/// The default document in tantivy. It encodes data in a compact form.
pub struct CompactDoc {
    /// `node_data` is a vec of bytes, where each value is serialized into bytes and stored. It
    /// includes all the data of the document and also metadata like where the nodes are located
    /// in an object or array.
    pub node_data: Vec<u8>,
    /// The root (Field, Value) pairs
    field_values: Vec<FieldValueAddr>,
}

impl Default for CompactDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactDoc {
    /// Creates a new, empty document object
    /// The reserved capacity is for the total serialized data
    pub fn with_capacity(bytes: usize) -> CompactDoc {
        CompactDoc {
            node_data: Vec::with_capacity(bytes),
            field_values: Vec::with_capacity(4),
        }
    }

    /// Skrinks the capacity of the document to fit the data
    pub fn shrink_to_fit(&mut self) {
        self.node_data.shrink_to_fit();
        self.field_values.shrink_to_fit();
    }

    /// Creates a new, empty document object
    pub fn new() -> CompactDoc {
        CompactDoc::with_capacity(1024)
    }

    /// Returns the length of the document.
    pub fn len(&self) -> usize {
        self.field_values.len()
    }

    /// Adding a facet to the document.
    pub fn add_facet<F>(&mut self, field: Field, path: F)
    where Facet: From<F> {
        let facet = Facet::from(path);
        self.add_leaf_field_value(field, ReferenceValueLeaf::Facet(facet.encoded_str()));
    }

    /// Add a text field.
    pub fn add_text<S: AsRef<str>>(&mut self, field: Field, text: S) {
        self.add_leaf_field_value(field, ReferenceValueLeaf::Str(text.as_ref()));
    }

    /// Add a pre-tokenized text field.
    pub fn add_pre_tokenized_text(&mut self, field: Field, pre_tokenized_text: PreTokenizedString) {
        self.add_leaf_field_value(field, pre_tokenized_text);
    }

    /// Add a u64 field
    pub fn add_u64(&mut self, field: Field, value: u64) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a IP address field. Internally only Ipv6Addr is used.
    pub fn add_ip_addr(&mut self, field: Field, value: Ipv6Addr) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a i64 field
    pub fn add_i64(&mut self, field: Field, value: i64) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a f64 field
    pub fn add_f64(&mut self, field: Field, value: f64) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a bool field
    pub fn add_bool(&mut self, field: Field, value: bool) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a date field with unspecified time zone offset
    pub fn add_date(&mut self, field: Field, value: DateTime) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a bytes field
    pub fn add_bytes(&mut self, field: Field, value: &[u8]) {
        self.add_leaf_field_value(field, value);
    }

    /// Add a dynamic object field
    pub fn add_object(&mut self, field: Field, object: BTreeMap<String, OwnedValue>) {
        self.add_field_value(field, &OwnedValue::from(object));
    }

    /// Add a (field, value) to the document.
    ///
    /// `OwnedValue` implements Value, which should be easiest to use, but is not the most
    /// performant.
    pub fn add_field_value<'a, V: Value<'a>>(&mut self, field: Field, value: V) {
        let field_value = FieldValueAddr {
            field: field
                .field_id()
                .try_into()
                .expect("support only up to u16::MAX field ids"),
            value: self.add_value(value),
        };
        self.field_values.push(field_value);
    }

    /// Add a (field, leaf value) to the document.
    /// Leaf values don't have nested values.
    pub fn add_leaf_field_value<'a, T: Into<ReferenceValueLeaf<'a>>>(
        &mut self,
        field: Field,
        typed_val: T,
    ) {
        let value = typed_val.into();
        let field_value = FieldValueAddr {
            field: field
                .field_id()
                .try_into()
                .expect("support only up to u16::MAX field ids"),
            value: self.add_value_leaf(value),
        };
        self.field_values.push(field_value);
    }

    /// field_values accessor
    pub fn field_values(
        &self,
    ) -> impl Iterator<Item = (Field, ReferenceValue<'_, CompactDocValue<'_>>)> {
        self.field_values.iter().map(|field_val| {
            let field = Field::from_field_id(field_val.field as u32);
            let val = self.extract_ref_value(field_val.value).unwrap();
            (field, val)
        })
    }

    /// Returns all of the `ReferenceValue`s associated the given field
    pub fn get_all(
        &self,
        field: Field,
    ) -> impl Iterator<Item = ReferenceValue<'_, CompactDocValue<'_>>> + '_ {
        self.field_values
            .iter()
            .filter(move |field_value| Field::from_field_id(field_value.field as u32) == field)
            .map(|val| self.extract_ref_value(val.value).unwrap())
    }

    /// Returns the first `ReferenceValue` associated the given field
    pub fn get_first(&self, field: Field) -> Option<ReferenceValue<'_, CompactDocValue<'_>>> {
        self.get_all(field).next()
    }

    /// Create document from a named doc.
    pub fn convert_named_doc(
        schema: &Schema,
        named_doc: NamedFieldDocument,
    ) -> Result<Self, DocParsingError> {
        let mut document = Self::new();
        for (field_name, values) in named_doc.0 {
            if let Ok(field) = schema.get_field(&field_name) {
                for value in values {
                    document.add_field_value(field, &value);
                }
            }
        }
        Ok(document)
    }

    /// Build a document object from a json-object.
    pub fn parse_json(schema: &Schema, doc_json: &str) -> Result<Self, DocParsingError> {
        let json_obj: Map<String, serde_json::Value> =
            serde_json::from_str(doc_json).map_err(|_| DocParsingError::invalid_json(doc_json))?;
        Self::from_json_object(schema, json_obj)
    }

    /// Build a document object from a json-object.
    pub fn from_json_object(
        schema: &Schema,
        json_obj: Map<String, serde_json::Value>,
    ) -> Result<Self, DocParsingError> {
        let mut doc = Self::default();
        for (field_name, json_value) in json_obj {
            if let Ok(field) = schema.get_field(&field_name) {
                let field_entry = schema.get_field_entry(field);
                let field_type = field_entry.field_type();
                match json_value {
                    serde_json::Value::Array(json_items) => {
                        for json_item in json_items {
                            let value = field_type
                                .value_from_json(json_item)
                                .map_err(|e| DocParsingError::ValueError(field_name.clone(), e))?;
                            doc.add_field_value(field, &value);
                        }
                    }
                    _ => {
                        let value = field_type
                            .value_from_json(json_value)
                            .map_err(|e| DocParsingError::ValueError(field_name.clone(), e))?;
                        doc.add_field_value(field, &value);
                    }
                }
            }
        }
        Ok(doc)
    }
}

impl PartialEq for CompactDoc {
    fn eq(&self, other: &Self) -> bool {
        // super slow, but only here for tests
        let convert_to_comparable_map = |doc: &CompactDoc| {
            let mut field_value_set: HashMap<Field, HashSet<String>> = Default::default();
            for field_value in doc.field_values.iter() {
                let value: OwnedValue = doc.extract_ref_value(field_value.value).unwrap().into();
                let value = serde_json::to_string(&value).unwrap();
                field_value_set
                    .entry(Field::from_field_id(field_value.field as u32))
                    .or_default()
                    .insert(value);
            }
            field_value_set
        };
        let self_field_values: HashMap<Field, HashSet<String>> = convert_to_comparable_map(self);
        let other_field_values: HashMap<Field, HashSet<String>> = convert_to_comparable_map(other);
        self_field_values.eq(&other_field_values)
    }
}

impl Eq for CompactDoc {}

impl DocumentDeserialize for CompactDoc {
    fn deserialize<'de, D>(mut deserializer: D) -> Result<Self, DeserializeError>
    where D: DocumentDeserializer<'de> {
        let mut doc = CompactDoc::default();
        // TODO: Deserializing into OwnedValue is wasteful. The deserializer should be able to work
        // on slices and referenced data.
        while let Some((field, value)) = deserializer.next_field::<OwnedValue>()? {
            doc.add_field_value(field, &value);
        }
        Ok(doc)
    }
}

/// A value of Compact Doc needs a reference to the container to extract its payload
#[derive(Debug, Clone, Copy)]
pub struct CompactDocValue<'a> {
    container: &'a CompactDoc,
    value: ValueAddr,
}
impl<'a> Value<'a> for CompactDocValue<'a> {
    type ArrayIter = CompactDocArrayIter<'a>;

    type ObjectIter = CompactDocObjectIter<'a>;

    fn as_value(&self) -> ReferenceValue<'a, Self> {
        self.container.extract_ref_value(self.value).unwrap()
    }
}

#[derive(Clone, Copy, Default)]
/// The value type and the address to its payload in the container.
/// Since Addr is only 3 bytes, the struct size is only 4bytes
pub struct ValueAddr {
    type_id: ValueType,
    val: Addr, // this is the address, except for bool and null, which are inlined
}
impl BinarySerializable for ValueAddr {
    fn serialize<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        (self.type_id as u8).serialize(writer)?;
        self.val.0.serialize(writer)
    }

    fn deserialize<R: Read>(reader: &mut R) -> io::Result<Self> {
        let type_id = ValueType::deserialize(reader)?;
        let addr: [u8; 3] = <[u8; 3]>::deserialize(reader)?;
        Ok(ValueAddr {
            type_id,
            val: Addr(addr),
        })
    }
}
impl std::fmt::Debug for ValueAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{:?} at {:?}",
            self.type_id,
            u32::from(self.val)
        ))
    }
}
/// Addr in 3 bytes, can be converted from u32 by dropping the high byte.
/// This means that we can address at most 16MB data in a Document.
#[derive(Clone, Default, Eq, PartialEq, Debug, Copy)]
struct Addr([u8; 3]);
impl Addr {
    fn from_u32(val: u32) -> io::Result<Self> {
        if val >= 1 << 24 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Value too large for Addr, the default TantivyDocument Document supports up to \
                 16MB of payload",
            ));
        }
        let bytes = val.to_be_bytes();
        Ok(Addr([bytes[1], bytes[2], bytes[3]]))
    }
}
impl From<Addr> for u32 {
    fn from(val: Addr) -> Self {
        let mut bytes = [0; 4];
        bytes[0] = 0;
        bytes[1..].copy_from_slice(&val.0);
        u32::from_be_bytes(bytes)
    }
}

impl ValueAddr {
    pub fn new(type_id: ValueType, val: u32) -> Self {
        Self {
            type_id,
            val: Addr::from_u32(val).unwrap(),
        }
    }
}

/// A enum representing a leaf value for tantivy to index.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum ValueType {
    /// A null value.
    #[default]
    Null = 0,
    /// The str type is used for any text information.
    Str = 1,
    /// Unsigned 64-bits Integer `u64`
    U64 = 2,
    /// Signed 64-bits Integer `i64`
    I64 = 3,
    /// 64-bits Float `f64`
    F64 = 4,
    /// Date/time with nanoseconds precision
    Date = 5,
    /// Facet
    Facet = 6,
    /// Arbitrarily sized byte array
    Bytes = 7,
    /// IpV6 Address. Internally there is no IpV4, it needs to be converted to `Ipv6Addr`.
    IpAddr = 8,
    /// Bool value
    Bool = 9,
    /// Pre-tokenized str type,
    PreTokStr = 10,
    /// Object
    Object = 11,
    /// Pre-tokenized str type,
    Array = 12,
}

impl BinarySerializable for ValueType {
    fn serialize<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        (*self as u8).serialize(writer)?;
        Ok(())
    }

    fn deserialize<R: Read>(reader: &mut R) -> io::Result<Self> {
        let num = u8::deserialize(reader)?;
        let type_id = if (0..=12).contains(&num) {
            unsafe { std::mem::transmute(num) }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid value type id: {}", num),
            ));
        };
        Ok(type_id)
    }
}

impl<'a, V: Value<'a>> From<&ReferenceValue<'a, V>> for ValueType {
    fn from(value: &ReferenceValue<'a, V>) -> Self {
        match value {
            ReferenceValue::Leaf(leaf) => leaf.into(),
            ReferenceValue::Array(_) => ValueType::Array,
            ReferenceValue::Object(_) => ValueType::Object,
        }
    }
}
impl<'a> From<&ReferenceValueLeaf<'a>> for ValueType {
    fn from(value: &ReferenceValueLeaf<'a>) -> Self {
        match value {
            ReferenceValueLeaf::Null => ValueType::Null,
            ReferenceValueLeaf::Str(_) => ValueType::Str,
            ReferenceValueLeaf::U64(_) => ValueType::U64,
            ReferenceValueLeaf::I64(_) => ValueType::I64,
            ReferenceValueLeaf::F64(_) => ValueType::F64,
            ReferenceValueLeaf::Bool(_) => ValueType::Bool,
            ReferenceValueLeaf::Date(_) => ValueType::Date,
            ReferenceValueLeaf::IpAddr(_) => ValueType::IpAddr,
            ReferenceValueLeaf::PreTokStr(_) => ValueType::PreTokStr,
            ReferenceValueLeaf::Facet(_) => ValueType::Facet,
            ReferenceValueLeaf::Bytes(_) => ValueType::Bytes,
        }
    }
}

impl CompactDoc {
    pub(crate) fn add_value_leaf(&mut self, leaf: ReferenceValueLeaf) -> ValueAddr {
        let type_id = ValueType::from(&leaf);
        match leaf {
            ReferenceValueLeaf::Null => ValueAddr::new(type_id, 0),
            ReferenceValueLeaf::Str(bytes) => ValueAddr::new(
                type_id,
                write_bytes_into(&mut self.node_data, bytes.as_bytes()),
            ),
            ReferenceValueLeaf::Facet(bytes) => ValueAddr::new(
                type_id,
                write_bytes_into(&mut self.node_data, bytes.as_bytes()),
            ),
            ReferenceValueLeaf::Bytes(bytes) => {
                ValueAddr::new(type_id, write_bytes_into(&mut self.node_data, bytes))
            }
            ReferenceValueLeaf::U64(num) => {
                ValueAddr::new(type_id, write_into(&mut self.node_data, num))
            }
            ReferenceValueLeaf::I64(num) => {
                ValueAddr::new(type_id, write_into(&mut self.node_data, num))
            }
            ReferenceValueLeaf::F64(num) => {
                ValueAddr::new(type_id, write_into(&mut self.node_data, num))
            }
            ReferenceValueLeaf::Bool(b) => ValueAddr::new(type_id, b as u32),
            ReferenceValueLeaf::Date(date) => ValueAddr::new(
                type_id,
                write_into(&mut self.node_data, date.into_timestamp_nanos()),
            ),
            ReferenceValueLeaf::IpAddr(num) => {
                ValueAddr::new(type_id, write_into(&mut self.node_data, num.to_u128()))
            }
            ReferenceValueLeaf::PreTokStr(pre_tok) => {
                ValueAddr::new(type_id, write_into(&mut self.node_data, *pre_tok))
            }
        }
    }
    pub(crate) fn add_value<'a, V: Value<'a>>(&mut self, value: V) -> ValueAddr {
        let value = value.as_value();
        let type_id = ValueType::from(&value);
        match value {
            ReferenceValue::Leaf(leaf) => self.add_value_leaf(leaf),
            ReferenceValue::Array(elements) => {
                let mut positions = Vec::new();
                for elem in elements {
                    let ref_elem = self.add_value(elem);
                    let position = self.node_data.len() as u32;
                    write_u32_vint(position, &mut positions).expect("in memory can't fail");
                    write_into(&mut self.node_data, ref_elem);
                    // self.nodes.push(ref_elem);
                }
                ValueAddr::new(type_id, write_bytes_into(&mut self.node_data, &positions))
            }
            ReferenceValue::Object(entries) => {
                let mut positions = Vec::new();
                for (key, value) in entries {
                    let ref_key = self.add_value_leaf(ReferenceValueLeaf::Str(key));
                    let ref_value = self.add_value(value);
                    let position = self.node_data.len() as u32;
                    write_u32_vint(position, &mut positions).expect("in memory can't fail");
                    write_into(&mut self.node_data, ref_key);
                    write_into(&mut self.node_data, ref_value);
                }
                ValueAddr::new(type_id, write_bytes_into(&mut self.node_data, &positions))
            }
        }
    }

    pub(crate) fn extract_ref_value(
        &self,
        ref_value: ValueAddr,
    ) -> io::Result<ReferenceValue<'_, CompactDocValue<'_>>> {
        match ref_value.type_id {
            ValueType::Null => Ok(ReferenceValueLeaf::Null.into()),
            ValueType::Str => {
                let str_ref = binary_deserialize_str(self.get_slice(ref_value.val));
                Ok(ReferenceValueLeaf::Str(str_ref).into())
            }
            ValueType::Facet => {
                let str_ref = binary_deserialize_str(self.get_slice(ref_value.val));
                Ok(ReferenceValueLeaf::Facet(str_ref).into())
            }
            ValueType::Bytes => {
                let data = binary_deserialize_bytes(self.get_slice(ref_value.val));
                Ok(ReferenceValueLeaf::Bytes(data).into())
            }
            ValueType::U64 => self
                .read_from::<u64>(ref_value.val)
                .map(ReferenceValueLeaf::U64)
                .map(Into::into),
            ValueType::I64 => self
                .read_from::<i64>(ref_value.val)
                .map(ReferenceValueLeaf::I64)
                .map(Into::into),
            ValueType::F64 => self
                .read_from::<f64>(ref_value.val)
                .map(ReferenceValueLeaf::F64)
                .map(Into::into),
            ValueType::Bool => Ok(ReferenceValueLeaf::Bool(u32::from(ref_value.val) != 0).into()),
            ValueType::Date => self
                .read_from::<i64>(ref_value.val)
                .map(|ts| ReferenceValueLeaf::Date(DateTime::from_timestamp_nanos(ts)))
                .map(Into::into),
            ValueType::IpAddr => self
                .read_from::<u128>(ref_value.val)
                .map(|num| ReferenceValueLeaf::IpAddr(Ipv6Addr::from_u128(num)))
                .map(Into::into),
            ValueType::PreTokStr => self
                .read_from::<PreTokenizedString>(ref_value.val)
                .map(Into::into)
                .map(ReferenceValueLeaf::PreTokStr)
                .map(Into::into),
            ValueType::Object => Ok(ReferenceValue::Object(CompactDocObjectIter::new(
                self,
                ref_value.val,
            )?)),
            ValueType::Array => Ok(ReferenceValue::Array(CompactDocArrayIter::new(
                self,
                ref_value.val,
            )?)),
        }
    }

    pub(crate) fn extract_str(&self, ref_value: ValueAddr) -> &str {
        binary_deserialize_str(self.get_slice(ref_value.val))
    }

    fn read_from<T: BinarySerializable>(&self, addr: Addr) -> io::Result<T> {
        let start = u32::from(addr) as usize;
        let data_slice = &self.node_data[start..];
        let mut cursor = std::io::Cursor::new(data_slice);
        T::deserialize(&mut cursor)
    }

    fn get_slice(&self, addr: Addr) -> &[u8] {
        let start = u32::from(addr) as usize;
        &self.node_data[start..]
    }
}

/// BinarySerializable alternative to read references
fn binary_deserialize_str(data: &[u8]) -> &str {
    let data = binary_deserialize_bytes(data);
    unsafe { std::str::from_utf8_unchecked(data) }
}

/// BinarySerializable alternative to read references
fn binary_deserialize_bytes(data: &[u8]) -> &[u8] {
    let (len, bytes_read) = read_u32_vint_no_advance(data);
    &data[bytes_read..bytes_read + len as usize]
}

/// BinarySerializable alternative to write references
fn write_bytes_into(vec: &mut Vec<u8>, bytes: &[u8]) -> u32 {
    let pos = vec.len() as u32;
    let mut buf = [0u8; 8];
    let vint_bytes = serialize_vint_u32(bytes.len() as u32, &mut buf);
    vec.extend_from_slice(vint_bytes);
    vec.extend_from_slice(bytes);
    pos
}

/// Serialize and return the position
fn write_into<T: BinarySerializable>(vec: &mut Vec<u8>, value: T) -> u32 {
    let pos = vec.len() as u32;
    value.serialize(vec).unwrap();
    pos
}

#[derive(Debug, Clone)]
/// The Iterator for the object values in the compact document
pub struct CompactDocObjectIter<'a> {
    container: &'a CompactDoc,
    positions_slice: &'a [u8],
}

impl<'a> CompactDocObjectIter<'a> {
    fn new(container: &'a CompactDoc, addr: Addr) -> io::Result<Self> {
        let positions_slice = binary_deserialize_bytes(container.get_slice(addr));
        Ok(Self {
            container,
            positions_slice,
        })
    }
}

impl<'a> Iterator for CompactDocObjectIter<'a> {
    type Item = (&'a str, CompactDocValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if !self.positions_slice.is_empty() {
            let key_index = read_u32_vint(&mut self.positions_slice) as usize;
            let position = &mut &self.container.node_data[key_index..];
            let key_addr = ValueAddr::deserialize(position).ok()?;
            let key = self.container.extract_str(key_addr);
            let value = ValueAddr::deserialize(position).ok()?;
            let value = CompactDocValue {
                container: self.container,
                value,
            };
            return Some((key, value));
        }
        None
    }
}

#[derive(Debug, Clone)]
/// The Iterator for the array values in the compact document
pub struct CompactDocArrayIter<'a> {
    container: &'a CompactDoc,
    positions_slice: &'a [u8],
}

impl<'a> CompactDocArrayIter<'a> {
    fn new(container: &'a CompactDoc, addr: Addr) -> io::Result<Self> {
        let positions_slice = binary_deserialize_bytes(container.get_slice(addr));
        Ok(Self {
            container,
            positions_slice,
        })
    }
}

impl<'a> Iterator for CompactDocArrayIter<'a> {
    type Item = CompactDocValue<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.positions_slice.is_empty() {
            let key_index = read_u32_vint(&mut self.positions_slice) as usize;
            let value = ValueAddr::deserialize(&mut &self.container.node_data[key_index..]).ok()?;
            let value = CompactDocValue {
                container: self.container,
                value,
            };
            return Some(value);
        }
        None
    }
}

impl Document for CompactDoc {
    type Value<'a> = CompactDocValue<'a>;
    type FieldsValuesIter<'a> = FieldValueIterRef<'a>;

    fn iter_fields_and_values(&self) -> Self::FieldsValuesIter<'_> {
        FieldValueIterRef {
            slice: self.field_values.iter(),
            container: &self,
        }
    }
}

/// A helper wrapper for creating standard iterators
/// out of the fields iterator trait.
pub struct FieldValueIterRef<'a> {
    slice: std::slice::Iter<'a, FieldValueAddr>,
    container: &'a CompactDoc,
}

impl<'a> Iterator for FieldValueIterRef<'a> {
    type Item = (Field, CompactDocValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        self.slice.next().map(|field_value| {
            (
                Field::from_field_id(field_value.field as u32),
                CompactDocValue::<'a> {
                    container: self.container,
                    value: field_value.value,
                },
            )
        })
    }
}

/// Error that may happen when deserializing
/// a document from JSON.
#[derive(Debug, Error, PartialEq)]
pub enum DocParsingError {
    /// The payload given is not valid JSON.
    #[error("The provided string is not valid JSON")]
    InvalidJson(String),
    /// One of the value node could not be parsed.
    #[error("The field '{0:?}' could not be parsed: {1:?}")]
    ValueError(String, ValueParsingError),
}

impl DocParsingError {
    /// Builds a NotJson DocParsingError
    fn invalid_json(invalid_json: &str) -> Self {
        let sample = invalid_json.chars().take(20).collect();
        DocParsingError::InvalidJson(sample)
    }
}

#[cfg(test)]
mod tests {
    use crate::schema::*;

    #[test]
    fn test_doc() {
        let mut schema_builder = Schema::builder();
        let text_field = schema_builder.add_text_field("title", TEXT);
        let mut doc = TantivyDocument::default();
        doc.add_text(text_field, "My title");
        assert_eq!(doc.field_values().count(), 1);

        let schema = schema_builder.build();
        let _val = doc.get_first(text_field).unwrap();
        let _json = doc.to_named_doc(&schema);
    }

    #[test]
    fn test_json_value() {
        let json_str = r#"{ 
            "toto": "titi",
            "float": -0.2,
            "bool": true,
            "unsigned": 1,
            "signed": -2,
            "complexobject": {
                "field.with.dot": 1
            },
            "date": "1985-04-12T23:20:50.52Z",
            "my_arr": [2, 3, {"my_key": "two tokens"}, 4, {"nested_array": [2, 5, 6]}]
        }"#;
        let json_val: std::collections::BTreeMap<String, OwnedValue> =
            serde_json::from_str(json_str).unwrap();

        let mut schema_builder = Schema::builder();
        let json_field = schema_builder.add_json_field("json", TEXT);
        let mut doc = TantivyDocument::default();
        doc.add_object(json_field, json_val);

        let schema = schema_builder.build();
        let json = doc.to_json(&schema);
        let actual_json: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected_json: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(actual_json["json"][0], expected_json);
    }

    // TODO: Should this be re-added with the serialize method
    //       technically this is no longer useful since the doc types
    //       do not implement BinarySerializable due to orphan rules.
    // #[test]
    // fn test_doc_serialization_issue() {
    //     let mut doc = Document::default();
    //     doc.add_json_object(
    //         Field::from_field_id(0),
    //         serde_json::json!({"key": 2u64})
    //             .as_object()
    //             .unwrap()
    //             .clone(),
    //     );
    //     doc.add_text(Field::from_field_id(1), "hello");
    //     assert_eq!(doc.field_values().len(), 2);
    //     let mut payload: Vec<u8> = Vec::new();
    //     doc_binary_wrappers::serialize(&doc, &mut payload).unwrap();
    //     assert_eq!(payload.len(), 26);
    //     doc_binary_wrappers::deserialize::<Document, _>(&mut &payload[..]).unwrap();
    // }
}
