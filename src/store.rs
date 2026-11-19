use std::cmp::Ordering;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};
use std::path::Path;

use serde::Serialize;
use tracing::warn;

const EMPTY_STR_ID: u32 = 0;
const SNAPSHOT_MAGIC: &[u8; 8] = b"BINLKUP1";
const SNAPSHOT_HEADER_BYTES: usize = SNAPSHOT_MAGIC.len() + 6 * std::mem::size_of::<u32>();
const STORED_BIN_FIELDS: usize = 9;
const STORED_BIN_BYTES: usize = STORED_BIN_FIELDS * std::mem::size_of::<u32>();

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinInfo<'a> {
    #[serde(rename = "BIN")]
    pub bin: &'a str,
    #[serde(rename = "Brand")]
    pub brand: &'a str,
    #[serde(rename = "Type")]
    pub card_type: &'a str,
    #[serde(rename = "Category")]
    pub category: &'a str,
    #[serde(rename = "Issuer")]
    pub issuer: &'a str,
    #[serde(rename = "IssuerPhone")]
    pub issuer_phone: &'a str,
    #[serde(rename = "IssuerUrl")]
    pub issuer_url: &'a str,
    #[serde(rename = "isoCode2")]
    pub iso_code2: &'a str,
    #[serde(rename = "isoCode3")]
    pub iso_code3: &'a str,
    #[serde(rename = "CountryName")]
    pub country_name: &'a str,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Csv(csv::Error),
    MissingColumn(&'static str),
    CapacityExceeded,
    InvalidSnapshot(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read BIN dataset: {err}"),
            Self::Csv(err) => write!(f, "failed to parse BIN CSV: {err}"),
            Self::MissingColumn(col) => write!(f, "missing required CSV column: {col}"),
            Self::CapacityExceeded => write!(f, "store capacity exceeded"),
            Self::InvalidSnapshot(msg) => write!(f, "invalid BIN snapshot: {msg}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Csv(err) => Some(err),
            Self::MissingColumn(_) | Self::CapacityExceeded | Self::InvalidSnapshot(_) => None,
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<csv::Error> for StoreError {
    fn from(value: csv::Error) -> Self {
        Self::Csv(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StoredBin {
    brand: u32,
    card_type: u32,
    category: u32,
    issuer: u32,
    issuer_phone: u32,
    issuer_url: u32,
    iso_code2: u32,
    iso_code3: u32,
    country_name: u32,
}

impl StoredBin {
    fn fields(self) -> [u32; STORED_BIN_FIELDS] {
        [
            self.brand,
            self.card_type,
            self.category,
            self.issuer,
            self.issuer_phone,
            self.issuer_url,
            self.iso_code2,
            self.iso_code3,
            self.country_name,
        ]
    }

    fn from_fields(fields: [u32; STORED_BIN_FIELDS]) -> Self {
        Self {
            brand: fields[0],
            card_type: fields[1],
            category: fields[2],
            issuer: fields[3],
            issuer_phone: fields[4],
            issuer_url: fields[5],
            iso_code2: fields[6],
            iso_code3: fields[7],
            country_name: fields[8],
        }
    }
}

struct ColumnIndices {
    bin: usize,
    brand: usize,
    card_type: usize,
    category: usize,
    issuer: usize,
    issuer_phone: usize,
    issuer_url: usize,
    iso_code2: usize,
    iso_code3: usize,
    country_name: usize,
}

impl ColumnIndices {
    fn from_headers(headers: &csv::StringRecord) -> Result<Self, StoreError> {
        let find = |name: &'static str| -> Result<usize, StoreError> {
            headers
                .iter()
                .position(|h| h.trim_start_matches('\u{FEFF}').trim() == name)
                .ok_or(StoreError::MissingColumn(name))
        };

        Ok(Self {
            bin: find("BIN")?,
            brand: find("Brand")?,
            card_type: find("Type")?,
            category: find("Category")?,
            issuer: find("Issuer")?,
            issuer_phone: find("IssuerPhone")?,
            issuer_url: find("IssuerUrl")?,
            iso_code2: find("isoCode2")?,
            iso_code3: find("isoCode3")?,
            country_name: find("CountryName")?,
        })
    }
}

pub struct BinStore {
    arena: Box<str>,
    str_offsets: Box<[u32]>,
    records: Box<[StoredBin]>,
    key_arena: Box<str>,
    key_offsets: Box<[u32]>,
    entry_records: Box<[u32]>,
}

impl BinStore {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let mut prefix = [0u8; SNAPSHOT_MAGIC.len()];
        let prefix_len = read_prefix(&mut file, &mut prefix)?;
        if prefix_len == SNAPSHOT_MAGIC.len() && &prefix == SNAPSHOT_MAGIC {
            let mut bytes = Vec::with_capacity(usize::try_from(file_len).unwrap_or(0));
            bytes.extend_from_slice(&prefix);
            file.read_to_end(&mut bytes)?;
            return Self::from_snapshot_bytes(&bytes);
        }

        let estimated_rows = (file_len / 75) as usize;
        let chained = Cursor::new(prefix).take(prefix_len as u64).chain(file);
        Self::from_csv_reader_with_capacity(
            BufReader::with_capacity(256 * 1024, chained),
            estimated_rows,
        )
    }

    pub fn from_reader<R: Read>(mut reader: R) -> Result<Self, StoreError> {
        let mut prefix = [0u8; SNAPSHOT_MAGIC.len()];
        let prefix_len = read_prefix(&mut reader, &mut prefix)?;
        if prefix_len == SNAPSHOT_MAGIC.len() && &prefix == SNAPSHOT_MAGIC {
            let mut bytes = Vec::from(prefix);
            reader.read_to_end(&mut bytes)?;
            return Self::from_snapshot_bytes(&bytes);
        }

        let chained = Cursor::new(prefix).take(prefix_len as u64).chain(reader);
        Self::from_csv_reader_with_capacity(chained, 0)
    }

    pub fn write_snapshot<W: Write>(&self, mut writer: W) -> Result<(), StoreError> {
        let header_counts = [
            u32::try_from(self.arena.len()).map_err(|_| StoreError::CapacityExceeded)?,
            u32::try_from(self.str_offsets.len()).map_err(|_| StoreError::CapacityExceeded)?,
            u32::try_from(self.records.len()).map_err(|_| StoreError::CapacityExceeded)?,
            u32::try_from(self.key_arena.len()).map_err(|_| StoreError::CapacityExceeded)?,
            u32::try_from(self.key_offsets.len()).map_err(|_| StoreError::CapacityExceeded)?,
            u32::try_from(self.entry_records.len()).map_err(|_| StoreError::CapacityExceeded)?,
        ];

        writer.write_all(SNAPSHOT_MAGIC)?;
        write_u32_slice(&mut writer, &header_counts)?;
        writer.write_all(self.arena.as_bytes())?;
        write_u32_slice(&mut writer, &self.str_offsets)?;
        for record in &self.records {
            write_u32_slice(&mut writer, &record.fields())?;
        }
        writer.write_all(self.key_arena.as_bytes())?;
        write_u32_slice(&mut writer, &self.key_offsets)?;
        write_u32_slice(&mut writer, &self.entry_records)?;
        Ok(())
    }

    pub fn save_snapshot(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let file = File::create(path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, file);
        self.write_snapshot(&mut writer)?;
        writer.flush()?;
        Ok(())
    }

    pub fn from_snapshot_bytes(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() < SNAPSHOT_HEADER_BYTES {
            return Err(StoreError::InvalidSnapshot("truncated header"));
        }
        if &bytes[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
            return Err(StoreError::InvalidSnapshot("invalid magic header"));
        }

        let header = decode_u32_slice(&bytes[SNAPSHOT_MAGIC.len()..SNAPSHOT_HEADER_BYTES])?;
        let arena_len = header[0] as usize;
        let str_offsets_len = header[1] as usize;
        let records_len = header[2] as usize;
        let key_arena_len = header[3] as usize;
        let key_offsets_len = header[4] as usize;
        let entry_records_len = header[5] as usize;

        if str_offsets_len < 2 || key_offsets_len < 1 || entry_records_len != key_offsets_len - 1 {
            return Err(StoreError::InvalidSnapshot("invalid section counts"));
        }

        let u32_size = std::mem::size_of::<u32>();
        let expected = SNAPSHOT_HEADER_BYTES
            .checked_add(arena_len)
            .and_then(|v| {
                str_offsets_len
                    .checked_mul(u32_size)
                    .and_then(|n| v.checked_add(n))
            })
            .and_then(|v| {
                records_len
                    .checked_mul(STORED_BIN_BYTES)
                    .and_then(|n| v.checked_add(n))
            })
            .and_then(|v| v.checked_add(key_arena_len))
            .and_then(|v| {
                key_offsets_len
                    .checked_mul(u32_size)
                    .and_then(|n| v.checked_add(n))
            })
            .and_then(|v| {
                entry_records_len
                    .checked_mul(u32_size)
                    .and_then(|n| v.checked_add(n))
            })
            .ok_or(StoreError::InvalidSnapshot("snapshot size overflow"))?;

        if bytes.len() != expected {
            return Err(StoreError::InvalidSnapshot("unexpected snapshot size"));
        }

        let mut cursor = &bytes[SNAPSHOT_HEADER_BYTES..];

        let arena: Box<str> = std::str::from_utf8(take_slice(&mut cursor, arena_len)?)
            .map_err(|_| StoreError::InvalidSnapshot("invalid UTF-8 in string arena"))?
            .into();
        let str_offsets = decode_u32_slice(take_slice(&mut cursor, str_offsets_len * u32_size)?)?;

        let records_bytes = take_slice(&mut cursor, records_len * STORED_BIN_BYTES)?;
        let mut records = Vec::with_capacity(records_len);
        for chunk in records_bytes.as_chunks::<STORED_BIN_BYTES>().0 {
            let fields_vec = decode_u32_slice(chunk)?;
            let fields: [u32; STORED_BIN_FIELDS] = fields_vec
                .as_ref()
                .try_into()
                .map_err(|_| StoreError::InvalidSnapshot("invalid record width"))?;
            records.push(StoredBin::from_fields(fields));
        }
        let records = records.into_boxed_slice();

        let key_arena: Box<str> = std::str::from_utf8(take_slice(&mut cursor, key_arena_len)?)
            .map_err(|_| StoreError::InvalidSnapshot("invalid UTF-8 in key arena"))?
            .into();
        let key_offsets = decode_u32_slice(take_slice(&mut cursor, key_offsets_len * u32_size)?)?;
        let entry_records =
            decode_u32_slice(take_slice(&mut cursor, entry_records_len * u32_size)?)?;

        if str_offsets[0] != 0 || str_offsets[1] != 0 {
            return Err(StoreError::InvalidSnapshot("invalid empty string offsets"));
        }
        validate_offsets(&str_offsets, &arena)?;

        let max_str_id = u32::try_from(str_offsets.len() - 1)
            .map_err(|_| StoreError::InvalidSnapshot("string count overflow"))?;
        for record in &records {
            if record.fields().into_iter().any(|id| id >= max_str_id) {
                return Err(StoreError::InvalidSnapshot("string ID out of bounds"));
            }
        }

        if key_offsets[0] != 0 {
            return Err(StoreError::InvalidSnapshot("invalid key offsets"));
        }
        validate_offsets(&key_offsets, &key_arena)?;

        let max_record_id = u32::try_from(records.len())
            .map_err(|_| StoreError::InvalidSnapshot("record count overflow"))?;
        let mut prev_key: Option<&str> = None;
        for (idx, &record_id) in entry_records.iter().enumerate() {
            if record_id >= max_record_id {
                return Err(StoreError::InvalidSnapshot("record ID out of bounds"));
            }
            let start = key_offsets[idx] as usize;
            let end = key_offsets[idx + 1] as usize;
            let key = &key_arena[start..end];
            if let Some(prev) = prev_key
                && prev >= key
            {
                return Err(StoreError::InvalidSnapshot("keys are not strictly sorted"));
            }
            prev_key = Some(key);
        }

        Ok(Self {
            arena,
            str_offsets,
            records,
            key_arena,
            key_offsets,
            entry_records,
        })
    }

    fn from_csv_reader_with_capacity<R: Read>(
        reader: R,
        estimated_rows: usize,
    ) -> Result<Self, StoreError> {
        let mut csv_reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(reader);
        let columns = ColumnIndices::from_headers(csv_reader.headers()?)?;

        let mut arena = String::new();
        let mut str_offsets = vec![0u32, 0];
        let mut interner: HashMap<Box<str>, u32> = HashMap::new();
        let mut records: Vec<StoredBin> = Vec::new();
        let mut record_interner: HashMap<StoredBin, u32> = HashMap::new();
        let mut staging_keys = String::with_capacity(estimated_rows.saturating_mul(6));
        let mut staging: Vec<(u32, u32, u32)> = Vec::with_capacity(estimated_rows);
        let mut record = csv::StringRecord::new();

        while csv_reader.read_record(&mut record)? {
            let bin = record.get(columns.bin).unwrap_or("").trim();
            if bin.is_empty() {
                continue;
            }

            let stored = StoredBin {
                brand: intern(
                    record.get(columns.brand).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                card_type: intern(
                    record.get(columns.card_type).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                category: intern(
                    record.get(columns.category).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                issuer: intern(
                    record.get(columns.issuer).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                issuer_phone: intern(
                    record.get(columns.issuer_phone).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                issuer_url: intern(
                    record.get(columns.issuer_url).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                iso_code2: intern(
                    record.get(columns.iso_code2).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                iso_code3: intern(
                    record.get(columns.iso_code3).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
                country_name: intern(
                    record.get(columns.country_name).unwrap_or(""),
                    &mut arena,
                    &mut str_offsets,
                    &mut interner,
                )?,
            };

            let record_id = intern_record(stored, &mut records, &mut record_interner)?;
            let key_start = u32::try_from(staging_keys.len())
                .map_err(|_| StoreError::CapacityExceeded)?;
            let key_end = staging_keys
                .len()
                .checked_add(bin.len())
                .and_then(|total| u32::try_from(total).ok())
                .ok_or(StoreError::CapacityExceeded)?;
            staging_keys.push_str(bin);
            staging.push((key_start, key_end, record_id));
        }

        drop(interner);
        drop(record_interner);

        staging.sort_by(|a, b| {
            staging_keys[a.0 as usize..a.1 as usize].cmp(&staging_keys[b.0 as usize..b.1 as usize])
        });

        let mut key_arena = String::with_capacity(staging_keys.len());
        let mut key_offsets = Vec::with_capacity(staging.len() + 1);
        key_offsets.push(0u32);
        let mut entry_records = Vec::with_capacity(staging.len());
        let mut prev_start = 0usize;

        for &(start, end, record_id) in &staging {
            let bin = &staging_keys[start as usize..end as usize];
            if !entry_records.is_empty() && &key_arena[prev_start..] == bin {
                warn!(bin = %bin, "duplicate BIN entry; overwriting previous record");
                *entry_records.last_mut().unwrap() = record_id;
                continue;
            }

            prev_start = key_arena.len();
            let next_end = key_arena
                .len()
                .checked_add(bin.len())
                .and_then(|total| u32::try_from(total).ok())
                .ok_or(StoreError::CapacityExceeded)?;
            key_arena.push_str(bin);
            key_offsets.push(next_end);
            entry_records.push(record_id);
        }

        Ok(Self {
            arena: arena.into_boxed_str(),
            str_offsets: str_offsets.into_boxed_slice(),
            records: records.into_boxed_slice(),
            key_arena: key_arena.into_boxed_str(),
            key_offsets: key_offsets.into_boxed_slice(),
            entry_records: entry_records.into_boxed_slice(),
        })
    }

    fn key_at(&self, index: usize) -> &str {
        let start = self.key_offsets[index] as usize;
        let end = self.key_offsets[index + 1] as usize;
        &self.key_arena[start..end]
    }

    fn resolve_str(&self, id: u32) -> &str {
        let idx = id as usize;
        let start = self.str_offsets[idx] as usize;
        let end = self.str_offsets[idx + 1] as usize;
        &self.arena[start..end]
    }

    pub fn lookup(&self, bin: &str) -> Option<BinInfo<'_>> {
        let mut low = 0usize;
        let mut high = self.entry_records.len();
        while low < high {
            let mid = low + (high - low) / 2;
            match self.key_at(mid).cmp(bin) {
                Ordering::Less => low = mid + 1,
                Ordering::Greater => high = mid,
                Ordering::Equal => {
                    let stored = &self.records[self.entry_records[mid] as usize];
                    return Some(BinInfo {
                        bin: self.key_at(mid),
                        brand: self.resolve_str(stored.brand),
                        card_type: self.resolve_str(stored.card_type),
                        category: self.resolve_str(stored.category),
                        issuer: self.resolve_str(stored.issuer),
                        issuer_phone: self.resolve_str(stored.issuer_phone),
                        issuer_url: self.resolve_str(stored.issuer_url),
                        iso_code2: self.resolve_str(stored.iso_code2),
                        iso_code3: self.resolve_str(stored.iso_code3),
                        country_name: self.resolve_str(stored.country_name),
                    });
                }
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.entry_records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entry_records.is_empty()
    }

    pub fn memory_bytes(&self) -> usize {
        self.arena.len()
            + std::mem::size_of_val(self.str_offsets.as_ref())
            + std::mem::size_of_val(self.records.as_ref())
            + self.key_arena.len()
            + std::mem::size_of_val(self.key_offsets.as_ref())
            + std::mem::size_of_val(self.entry_records.as_ref())
    }
}

fn intern_record(
    stored: StoredBin,
    records: &mut Vec<StoredBin>,
    record_interner: &mut HashMap<StoredBin, u32>,
) -> Result<u32, StoreError> {
    if let Some(&id) = record_interner.get(&stored) {
        return Ok(id);
    }

    let id = u32::try_from(records.len()).map_err(|_| StoreError::CapacityExceeded)?;
    records.push(stored);
    record_interner.insert(stored, id);
    Ok(id)
}

fn intern(
    value: &str,
    arena: &mut String,
    str_offsets: &mut Vec<u32>,
    interner: &mut HashMap<Box<str>, u32>,
) -> Result<u32, StoreError> {
    if value.is_empty() {
        return Ok(EMPTY_STR_ID);
    }

    if let Some(&id) = interner.get(value) {
        return Ok(id);
    }

    u32::try_from(str_offsets.len()).map_err(|_| StoreError::CapacityExceeded)?;
    let id = (str_offsets.len() - 1) as u32;

    let end = arena
        .len()
        .checked_add(value.len())
        .and_then(|total| u32::try_from(total).ok())
        .ok_or(StoreError::CapacityExceeded)?;

    arena.push_str(value);
    str_offsets.push(end);
    interner.insert(value.into(), id);
    Ok(id)
}

fn read_prefix<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

fn write_u32_slice<W: Write>(writer: &mut W, values: &[u32]) -> std::io::Result<()> {
    for &value in values {
        writer.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

fn decode_u32_slice(bytes: &[u8]) -> Result<Box<[u32]>, StoreError> {
    if !bytes.len().is_multiple_of(std::mem::size_of::<u32>()) {
        return Err(StoreError::InvalidSnapshot("unaligned u32 section"));
    }
    let mut values = Vec::with_capacity(bytes.len() / std::mem::size_of::<u32>());
    for &chunk in bytes.as_chunks::<4>().0 {
        values.push(u32::from_le_bytes(chunk));
    }
    Ok(values.into_boxed_slice())
}

fn take_slice<'a>(cursor: &mut &'a [u8], len: usize) -> Result<&'a [u8], StoreError> {
    if cursor.len() < len {
        return Err(StoreError::InvalidSnapshot("unexpected snapshot size"));
    }
    let (head, tail) = cursor.split_at(len);
    *cursor = tail;
    Ok(head)
}

fn validate_offsets(offsets: &[u32], arena: &str) -> Result<(), StoreError> {
    let mut prev = 0usize;
    for &raw in offsets {
        let offset = raw as usize;
        if offset < prev || offset > arena.len() || !arena.is_char_boundary(offset) {
            return Err(StoreError::InvalidSnapshot("invalid arena offset"));
        }
        prev = offset;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_headers_and_preserves_leading_zeros() {
        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
002102,\"PRIVATE LABEL\",CREDIT,STANDARD,\"CHINA MERCHANTS BANK\",95555,https://english.cmbchina.com,CN,CHN,CHINA
411111,VISA,DEBIT,CLASSIC,\"CONOTOXIA SP. Z O.O\",,,PL,POL,POLAND
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store should load");
        assert_eq!(store.len(), 2);

        let info = store.lookup("002102").expect("002102 should exist");
        assert_eq!(
            info,
            BinInfo {
                bin: "002102",
                brand: "PRIVATE LABEL",
                card_type: "CREDIT",
                category: "STANDARD",
                issuer: "CHINA MERCHANTS BANK",
                issuer_phone: "95555",
                issuer_url: "https://english.cmbchina.com",
                iso_code2: "CN",
                iso_code3: "CHN",
                country_name: "CHINA",
            }
        );

        let visa = store.lookup("411111").expect("411111 should exist");
        assert_eq!(visa.issuer_phone, "");
        assert_eq!(visa.issuer_url, "");
        assert!(store.lookup("999999").is_none());
    }

    #[test]
    fn maps_columns_by_header_name_regardless_of_order() {
        let csv_data = "\
CountryName,isoCode3,isoCode2,IssuerUrl,IssuerPhone,Issuer,Category,Type,Brand,BIN
DENMARK,DNK,DK,http://www.vestjyskbank.dk,+4596632000,\"VESTJYSK BANK A/S\",CLASSIC,DEBIT,VISA/DANKORT,457173
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store should load");
        let info = store.lookup("457173").expect("457173 should exist");
        assert_eq!(info.brand, "VISA/DANKORT");
        assert_eq!(info.card_type, "DEBIT");
        assert_eq!(info.category, "CLASSIC");
        assert_eq!(info.issuer, "VESTJYSK BANK A/S");
        assert_eq!(info.issuer_phone, "+4596632000");
        assert_eq!(info.issuer_url, "http://www.vestjyskbank.dk");
        assert_eq!(info.iso_code2, "DK");
        assert_eq!(info.iso_code3, "DNK");
        assert_eq!(info.country_name, "DENMARK");
    }

    #[test]
    fn rejects_missing_required_header() {
        let csv_data = "BIN,Brand,Type\n411111,VISA,DEBIT\n";
        let err = BinStore::from_reader(csv_data.as_bytes())
            .err()
            .expect("missing headers should fail");
        assert!(matches!(err, StoreError::MissingColumn("Category")));
    }

    #[test]
    fn deduplicates_repeated_strings_in_arena() {
        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
400001,VISA,DEBIT,CLASSIC,TEST BANK,+12345,https://example.com,US,USA,UNITED STATES
400002,VISA,DEBIT,CLASSIC,TEST BANK,+12345,https://example.com,US,USA,UNITED STATES
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store should load");
        let expected_unique_bytes = "VISA".len()
            + "DEBIT".len()
            + "CLASSIC".len()
            + "TEST BANK".len()
            + "+12345".len()
            + "https://example.com".len()
            + "US".len()
            + "USA".len()
            + "UNITED STATES".len();
        assert_eq!(store.arena.len(), expected_unique_bytes);
        assert_eq!(store.records.len(), 1);
    }

    #[test]
    fn handles_utf8_bom_in_header() {
        let csv_data = "\
\u{FEFF}BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
457173,VISA/DANKORT,DEBIT,CLASSIC,\"VESTJYSK BANK A/S\",+4596632000,http://www.vestjyskbank.dk,DK,DNK,DENMARK
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("BOM CSV should load");
        let info = store.lookup("457173").expect("457173 should exist");
        assert_eq!(info.brand, "VISA/DANKORT");
        assert_eq!(info.issuer, "VESTJYSK BANK A/S");
    }

    #[test]
    fn loads_bundled_dataset() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/bin-list-data.csv");
        let store = BinStore::from_path(path).expect("bundled dataset should load");
        assert!(store.len() > 300_000);
        assert!(store.records.len() < store.len());
        assert!(store.memory_bytes() < 10 * 1024 * 1024);

        let info = store.lookup("002102").expect("002102 should exist");
        assert_eq!(info.issuer, "CHINA MERCHANTS BANK");

        assert!(store.lookup("000000").is_none());
        assert!(store.lookup("002103").is_none());
        assert!(store.lookup("9999999").is_none());

        let mut snapshot = Vec::new();
        store
            .write_snapshot(&mut snapshot)
            .expect("snapshot write should succeed");
        let reloaded = BinStore::from_snapshot_bytes(&snapshot)
            .expect("bundled snapshot should reload");
        assert_eq!(reloaded.len(), store.len());
        assert_eq!(reloaded.memory_bytes(), store.memory_bytes());
        assert_eq!(
            reloaded.lookup("002102"),
            store.lookup("002102")
        );
        assert_eq!(
            reloaded.lookup("999999"),
            store.lookup("999999")
        );
    }

    #[test]
    fn roundtrips_binary_snapshot() {
        let empty_csv = "BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName\n";
        let empty_store = BinStore::from_reader(empty_csv.as_bytes()).expect("empty store loads");
        let mut empty_bytes = Vec::new();
        empty_store
            .write_snapshot(&mut empty_bytes)
            .expect("empty snapshot writes");
        let reloaded_empty =
            BinStore::from_reader(empty_bytes.as_slice()).expect("empty snapshot auto-detects");
        assert!(reloaded_empty.is_empty());
        assert!(reloaded_empty.lookup("411111").is_none());

        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
002102,\"PRIVATE LABEL\",CREDIT,STANDARD,\"CHINA MERCHANTS BANK\",95555,https://english.cmbchina.com,CN,CHN,CHINA
411111,VISA,DEBIT,CLASSIC,\"CONOTOXIA SP. Z O.O\",,,PL,POL,POLAND
411112,VISA,DEBIT,CLASSIC,\"CONOTOXIA SP. Z O.O\",,,PL,POL,POLAND
1234567890123456789012345,MASTERCARD,CREDIT,WORLD,LONG KEY BANK,,,DE,DEU,GERMANY
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store loads");
        let mut bytes = Vec::new();
        store.write_snapshot(&mut bytes).expect("snapshot writes");

        let from_reader =
            BinStore::from_reader(bytes.as_slice()).expect("snapshot auto-detects from reader");
        let from_bytes =
            BinStore::from_snapshot_bytes(&bytes).expect("snapshot loads from byte slice");

        for loaded in [&from_reader, &from_bytes] {
            assert_eq!(loaded.len(), store.len());
            assert_eq!(loaded.memory_bytes(), store.memory_bytes());
            assert_eq!(loaded.lookup("002102"), store.lookup("002102"));
            assert_eq!(loaded.lookup("411111"), store.lookup("411111"));
            assert_eq!(loaded.lookup("411112"), store.lookup("411112"));
            assert_eq!(
                loaded.lookup("1234567890123456789012345"),
                store.lookup("1234567890123456789012345")
            );
            assert!(loaded.lookup("999999").is_none());
        }
    }

    #[test]
    fn rejects_corrupt_binary_snapshots() {
        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
400001,VISA,DEBIT,CLASSIC,BANK A,,,US,USA,UNITED STATES
400002,MASTERCARD,CREDIT,GOLD,BANK B,,,GB,GBR,UNITED KINGDOM
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store loads");
        let mut valid = Vec::new();
        store.write_snapshot(&mut valid).expect("snapshot writes");

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            BinStore::from_snapshot_bytes(&bad_magic),
            Err(StoreError::InvalidSnapshot(_))
        ));

        assert!(matches!(
            BinStore::from_snapshot_bytes(&valid[..8]),
            Err(StoreError::InvalidSnapshot(_))
        ));
        assert!(matches!(
            BinStore::from_snapshot_bytes(&valid[..valid.len() - 1]),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(matches!(
            BinStore::from_snapshot_bytes(&trailing),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let mut invalid_utf8 = valid.clone();
        invalid_utf8[SNAPSHOT_HEADER_BYTES] = 0xff;
        assert!(matches!(
            BinStore::from_snapshot_bytes(&invalid_utf8),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let arena_len = u32::from_le_bytes(valid[8..12].try_into().unwrap()) as usize;
        let str_offsets_len = u32::from_le_bytes(valid[12..16].try_into().unwrap()) as usize;
        let records_len = u32::from_le_bytes(valid[16..20].try_into().unwrap()) as usize;
        let key_arena_len = u32::from_le_bytes(valid[20..24].try_into().unwrap()) as usize;
        let key_offsets_len = u32::from_le_bytes(valid[24..28].try_into().unwrap()) as usize;

        let str_offsets_start = SNAPSHOT_HEADER_BYTES + arena_len;
        let records_start = str_offsets_start + str_offsets_len * 4;
        let key_arena_start = records_start + records_len * STORED_BIN_BYTES;
        let key_offsets_start = key_arena_start + key_arena_len;
        let entry_records_start = key_offsets_start + key_offsets_len * 4;

        let mut oob_str_id = valid.clone();
        oob_str_id[records_start..records_start + 4].copy_from_slice(&999_999u32.to_le_bytes());
        assert!(matches!(
            BinStore::from_snapshot_bytes(&oob_str_id),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let mut oob_record_id = valid.clone();
        oob_record_id[entry_records_start..entry_records_start + 4]
            .copy_from_slice(&999_999u32.to_le_bytes());
        assert!(matches!(
            BinStore::from_snapshot_bytes(&oob_record_id),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let mut non_monotonic_offsets = valid.clone();
        non_monotonic_offsets[str_offsets_start + 12..str_offsets_start + 16]
            .copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            BinStore::from_snapshot_bytes(&non_monotonic_offsets),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let mut unsorted_keys = valid.clone();
        unsorted_keys[key_arena_start..key_arena_start + 6].copy_from_slice(b"400002");
        unsorted_keys[key_arena_start + 6..key_arena_start + 12].copy_from_slice(b"400001");
        assert!(matches!(
            BinStore::from_snapshot_bytes(&unsorted_keys),
            Err(StoreError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn skips_blank_bins_overwrites_duplicates_and_supports_long_keys() {
        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
   ,VISA,DEBIT,CLASSIC,IGNORED,,,US,USA,UNITED STATES
411111,VISA,DEBIT,CLASSIC,FIRST BANK,,,US,USA,UNITED STATES
411111,VISA,CREDIT,GOLD,SECOND BANK,,,GB,GBR,UNITED KINGDOM
1234567890123456789012345,MASTERCARD,CREDIT,WORLD,LONG KEY BANK,,,DE,DEU,GERMANY
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store should load");
        assert_eq!(store.len(), 2);

        let updated = store.lookup("411111").expect("411111 should exist");
        assert_eq!(updated.issuer, "SECOND BANK");
        assert_eq!(updated.card_type, "CREDIT");
        assert_eq!(updated.country_name, "UNITED KINGDOM");

        let long_key = store
            .lookup("1234567890123456789012345")
            .expect("long key should exist");
        assert_eq!(long_key.brand, "MASTERCARD");
        assert_eq!(long_key.issuer, "LONG KEY BANK");
    }
}
