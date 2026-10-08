use std::{fmt, str::FromStr, sync::Arc};

use bigdecimal::BigDecimal;
use indexmap::IndexMap;
use thiserror::Error;

use crate::{DurationValue, Primitive, Provenance, Shape, Timestamp, numeric};
mod arithmetic;
pub use arithmetic::{DecimalOp, NumericError};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("duplicate field: {0}")]
    DuplicateField(String),
    #[error("data is inconsistent with shape {0}")]
    ShapeMismatch(String),
    #[error("invalid decimal: {0}")]
    InvalidDecimal(String),
    #[error("decimal scale is outside the supported signed 32-bit range")]
    DecimalScale,
    #[error("nanosecond adjustment must be in 0..1,000,000,000")]
    InvalidNanos,
    #[error("invalid timestamp or duration: {0}")]
    InvalidTime(String),
}

/// Exact decimal with scale-sensitive identity.
#[derive(Clone, Debug)]
pub struct Decimal(BigDecimal);

impl Decimal {
    /// Exact value with insignificant coefficient zeroes removed, for human presentation.
    pub fn normalized(&self) -> Self {
        if self.scale() <= 0 {
            return self.clone();
        }
        let value = self.0.normalized();
        // An originally fractional value already bounds expansion to scale zero.
        Self(if value.as_bigint_and_exponent().1 < 0 {
            value.with_scale(0)
        } else {
            value
        })
    }

    /// Conservative byte bound for this wrapper's compact Display format, without cloning or
    /// formatting the coefficient and without expanding an exponent-sized run of zeroes.
    pub fn compact_text_size_bound(&self) -> u64 {
        self.0.as_bigint_and_scale().0.bits().saturating_add(32)
    }
    pub(crate) fn number(&self) -> &BigDecimal {
        &self.0
    }
    pub fn scale(&self) -> i64 {
        self.0.as_bigint_and_exponent().1
    }
    pub fn numeric_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
    /// Exact plain decimal text, only after checking the expanded output size.
    /// The limit applies to the returned text, not allocator overhead or the existing coefficient.
    pub fn plain_text(&self, max_bytes: usize) -> Option<String> {
        let (coefficient, scale) = self.0.as_bigint_and_exponent();
        let coefficient = coefficient.to_string();
        if coefficient == "0" && scale <= 0 {
            return (max_bytes >= 1).then(|| "0".into());
        }
        let sign = usize::from(coefficient.starts_with('-'));
        let digits = coefficient.len() - sign;
        let length = if scale <= 0 {
            digits.checked_add(usize::try_from(-scale).ok()?)?
        } else if usize::try_from(scale).ok()? < digits {
            digits.checked_add(1)?
        } else {
            usize::try_from(scale).ok()?.checked_add(2)?
        };
        if length.checked_add(sign)? > max_bytes {
            return None;
        }
        // bigdecimal documents exponent-sized allocation in this method. The exact size check
        // above is required; handing it a bounded writer alone would not bound that allocation.
        Some(self.0.to_plain_string())
    }
    /// Exact integer conversion after multiplying by 10^decimal_places, without unbounded expansion.
    pub fn exact_scaled_i128(&self, decimal_places: u32) -> Option<i128> {
        let (coefficient, scale) = self.0.as_bigint_and_exponent();
        let scale = scale.checked_sub(i64::from(decimal_places))?;
        let coefficient = coefficient.to_string();
        if coefficient == "0" {
            return Some(0);
        }
        let (sign, digits) = coefficient
            .strip_prefix('-')
            .map_or(("", coefficient.as_str()), |digits| ("-", digits));
        let integer = if scale > 0 {
            let split = digits.len().checked_sub(usize::try_from(scale).ok()?)?;
            if split == 0 || digits[split..].bytes().any(|digit| digit != b'0') {
                return None;
            }
            digits[..split].to_owned()
        } else {
            let zeros = usize::try_from(-scale).ok()?;
            if digits.len().checked_add(zeros)? > 39 {
                return None;
            }
            let mut integer = digits.to_owned();
            integer.extend(std::iter::repeat_n('0', zeros));
            integer
        };
        format!("{sign}{integer}").parse().ok()
    }
    pub fn exact_i64(&self) -> Option<i64> {
        i64::try_from(self.exact_scaled_i128(0)?).ok()
    }
}

impl FromStr for Decimal {
    type Err = ModelError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let normalized = numeric::ascii_digits(text);
        if !numeric::DECIMAL.is_match(&normalized) {
            return Err(ModelError::InvalidDecimal(text.into()));
        }
        let number = BigDecimal::from_str(&normalized)
            .map_err(|_| ModelError::InvalidDecimal(text.into()))?;
        i32::try_from(number.as_bigint_and_exponent().1).map_err(|_| ModelError::DecimalScale)?;
        Ok(Self(number))
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bigint_and_exponent() == other.0.as_bigint_and_exponent()
    }
}
impl Eq for Decimal {}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (coefficient, scale) = self.0.as_bigint_and_exponent();
        let coefficient = coefficient.to_string();
        let digits = if let Some(digits) = coefficient.strip_prefix('-') {
            f.write_str("-")?;
            digits
        } else {
            &coefficient
        };
        let adjusted = digits.len() as i64 - scale - 1;
        if scale >= 0 && adjusted >= -6 {
            let point = digits.len() as i64 - scale;
            if point <= 0 {
                f.write_str("0.")?;
                for _ in 0..-point {
                    f.write_str("0")?;
                }
                f.write_str(digits)
            } else {
                let point = point as usize;
                f.write_str(&digits[..point])?;
                if point < digits.len() {
                    f.write_str(".")?;
                    f.write_str(&digits[point..])?;
                }
                Ok(())
            }
        } else {
            f.write_str(&digits[..1])?;
            if digits.len() > 1 {
                f.write_str(".")?;
                f.write_str(&digits[1..])?;
            }
            if adjusted >= 0 {
                write!(f, "E+{adjusted}")
            } else {
                write!(f, "E{adjusted}")
            }
        }
    }
}

/// Normalized seconds plus positive nanos, including negative durations.
/// Calendar parsing and Instant range validation belong to the temporal boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeParts {
    seconds: i64,
    nanos: u32,
}

impl TimeParts {
    pub fn new(seconds: i64, nanos: u32) -> Result<Self, ModelError> {
        if nanos >= 1_000_000_000 {
            return Err(ModelError::InvalidNanos);
        }
        Ok(Self { seconds, nanos })
    }
    pub fn seconds(self) -> i64 {
        self.seconds
    }
    pub fn nanos(self) -> u32 {
        self.nanos
    }
}

/// Content independent of its declared shape. Values expose only shared reads.
/// Text and byte payloads have immutable shared storage: cloning a tree may copy
/// its containers, but never copies those payload bytes. Logical budgets still
/// count their full length independently of physical allocation sharing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    Text(Arc<str>),
    Int(i64),
    Decimal(Decimal),
    Bool(bool),
    Instant(Timestamp),
    Duration(DurationValue),
    Interval(crate::Interval),
    Bytes(Arc<[u8]>),
    List(Vec<Data>),
    Option(Option<Box<Data>>),
    Iter(Arc<crate::IterValue>),
    Dataset(Arc<crate::DatasetRef>),
    Record(IndexMap<String, Data>),
}

impl Data {
    /// Read a declared data path without materializing a compound scalar as a record.
    pub fn project_path<'a>(&'a self, fields: &[String]) -> Option<std::borrow::Cow<'a, Data>> {
        let mut current = std::borrow::Cow::Borrowed(self);
        for field in fields {
            current = match current {
                std::borrow::Cow::Borrowed(Data::Record(values)) => {
                    std::borrow::Cow::Borrowed(values.get(field)?)
                }
                std::borrow::Cow::Borrowed(Data::Interval(interval)) => {
                    std::borrow::Cow::Owned(interval_field(*interval, field)?)
                }
                std::borrow::Cow::Owned(Data::Interval(interval)) => {
                    std::borrow::Cow::Owned(interval_field(interval, field)?)
                }
                _ => return None,
            };
        }
        Some(current)
    }

    /// Whether this value is inline data with no traversal recipe or dataset reference.
    /// Excessive nesting/work also refuses this boundary (128 levels / one million nodes).
    pub fn is_inline(&self) -> bool {
        self.snapshot_kind(false)
    }
    /// A finite value snapshot may contain immutable dataset descriptors. Resolving or
    /// protecting their bytes still requires the owning store and current actor authority.
    pub fn is_storable_snapshot(&self) -> bool {
        self.snapshot_kind(true)
    }
    fn snapshot_kind(&self, datasets: bool) -> bool {
        fn visit(data: &Data, depth: usize, left: &mut usize, datasets: bool) -> bool {
            if depth > 128 || *left == 0 {
                return false;
            }
            *left -= 1;
            match data {
                Data::Iter(_) => false,
                Data::Dataset(_) => datasets,
                Data::List(xs) => xs.iter().all(|x| visit(x, depth + 1, left, datasets)),
                Data::Record(xs) => xs.values().all(|x| visit(x, depth + 1, left, datasets)),
                Data::Option(Some(x)) => visit(x, depth + 1, left, datasets),
                Data::Text(_)
                | Data::Int(_)
                | Data::Decimal(_)
                | Data::Bool(_)
                | Data::Instant(_)
                | Data::Duration(_)
                | Data::Interval(_)
                | Data::Bytes(_)
                | Data::Option(None) => true,
            }
        }
        visit(self, 0, &mut 1_000_000, datasets)
    }

    /// An inexpensive constructor guard, deliberately not deep contract validation.
    pub fn fits_shallow(&self, shape: &Shape) -> bool {
        match (self, shape) {
            (_, Shape::Unknown) => true,
            (Self::Record(_), Shape::Meta(_)) => true,
            (Self::Text(_), Shape::Primitive(Primitive::Text))
            | (Self::Int(_), Shape::Primitive(Primitive::Int))
            | (Self::Decimal(_), Shape::Primitive(Primitive::Decimal))
            | (Self::Bool(_), Shape::Primitive(Primitive::Bool))
            | (Self::Instant(_), Shape::Primitive(Primitive::Instant))
            | (Self::Duration(_), Shape::Primitive(Primitive::Duration))
            | (Self::Interval(_), Shape::Primitive(Primitive::Interval))
            | (Self::Bytes(_), Shape::Primitive(Primitive::Bytes))
            | (Self::List(_), Shape::List(_)) => true,
            (Self::Option(_), Shape::Option(_)) => true,
            (Self::Iter(iter), Shape::Iter(item)) => iter.item_shape().is_assignable_to(item),
            (Self::Dataset(_), Shape::Dataset(_)) => true,
            (Self::Record(fields), Shape::Record(record)) => {
                record.fields().all(|(key, _)| fields.contains_key(key))
            }
            _ => false,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Value {
    shape: Arc<Shape>,
    data: Arc<Data>,
    provenance: Arc<Provenance>,
    // Live authority is process-local and is never encoded by data/storage codecs.
    authority: Option<Arc<str>>,
    metadata: Option<Arc<crate::contracts::metadata::ValueMetadata>>,
}

impl Value {
    pub fn new(shape: Shape, data: Data, provenance: Provenance) -> Result<Self, ModelError> {
        if !data.fits_shallow(&shape) {
            return Err(ModelError::ShapeMismatch(shape.to_string()));
        }
        let provenance = provenance.with_policy(&embedded_policy(&data));
        Ok(Self {
            shape: Arc::new(shape),
            data: Arc::new(data),
            provenance: Arc::new(provenance),
            authority: None,
            metadata: None,
        })
    }
    pub fn management(kind: crate::MetaType, projection: Data, authority: String) -> Self {
        let mut value = Self::new(Shape::Meta(kind), projection, Provenance::default())
            .expect("management projection");
        value.authority = Some(authority.into());
        value
    }
    pub fn management_authority(&self) -> Option<&str> {
        self.authority.as_deref()
    }
    /// Constant-time equality of a captured immutable snapshot, for read revalidation.
    pub fn same_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shape, &other.shape)
            && Arc::ptr_eq(&self.data, &other.data)
            && Arc::ptr_eq(&self.provenance, &other.provenance)
            && match (&self.metadata, &other.metadata) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
            && self.authority == other.authority
    }
    pub fn metadata(&self) -> Option<&crate::contracts::metadata::ValueMetadata> {
        self.metadata.as_deref()
    }
    pub fn with_metadata(
        &self,
        metadata: Option<crate::contracts::metadata::ValueMetadata>,
    ) -> Self {
        let mut value = self.clone();
        value.metadata = metadata.map(Arc::new);
        value
    }
    pub fn shape(&self) -> &Shape {
        &self.shape
    }
    pub fn data(&self) -> &Data {
        &self.data
    }
    /// Consume the value at an admitted materialization boundary, without copying
    /// its container tree. The caller separately owns its shape and attribution.
    pub fn into_data(self) -> Data {
        Arc::unwrap_or_clone(self.data)
    }
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Attribution changes share immutable data and shape rather than copying a result tree.
    pub fn with_provenance(&self, provenance: Provenance) -> Self {
        Self {
            shape: Arc::clone(&self.shape),
            data: Arc::clone(&self.data),
            provenance: Arc::new(provenance.with_policy(self.provenance.policy())),
            authority: self.authority.clone(),
            metadata: self.metadata.clone(),
        }
    }

    /// Shares immutable content; the caller must have performed any required deep check.
    pub fn with_shape(&self, shape: Shape) -> Result<Self, ModelError> {
        if ((self.shape.contains_meta() || shape.contains_meta()) && self.shape.as_ref() != &shape)
            || !self.data.fits_shallow(&shape)
        {
            return Err(ModelError::ShapeMismatch(shape.to_string()));
        }
        Ok(Self {
            shape: Arc::new(shape),
            data: Arc::clone(&self.data),
            provenance: Arc::clone(&self.provenance),
            authority: self.authority.clone(),
            metadata: self.metadata.clone(),
        })
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.provenance.policy().is_private() {
            f.write_str("Value(private; payload and attribution withheld)")
        } else {
            f.debug_struct("Value")
                .field("shape", &self.shape)
                .field("data", &self.data)
                .field("provenance", &self.provenance)
                .finish()
        }
    }
}

// Iter recipes retain a whole labelled Value inside Data. A caller cannot remove its restrictions
// by wrapping the recipe in a new list/record with empty attribution. Bound this constructor walk;
// exceeding the bound is conservatively private with unknown egress, never public by truncation.
fn embedded_policy(data: &Data) -> crate::flow::FlowPolicy {
    fn visit(data: &Data, depth: usize, left: &mut usize, policy: &mut crate::flow::FlowPolicy) {
        if depth > 256 || *left == 0 {
            *policy = policy.clone().private().unknown();
            return;
        }
        *left -= 1;
        match data {
            Data::Iter(iter) => *policy = policy.join(iter.source().provenance().policy()),
            Data::List(items) => {
                for item in items.iter().take(*left + 1) {
                    visit(item, depth + 1, left, policy);
                }
            }
            Data::Record(items) => {
                for item in items.values().take(*left + 1) {
                    visit(item, depth + 1, left, policy);
                }
            }
            Data::Option(Some(item)) => visit(item, depth + 1, left, policy),
            _ => {}
        }
    }
    let mut policy = crate::flow::FlowPolicy::default();
    visit(data, 0, &mut 1_000_000, &mut policy);
    policy
}

fn interval_field(interval: crate::Interval, name: &str) -> Option<Data> {
    match name {
        "start" => Some(Data::Instant(interval.start())),
        "end" => Some(Data::Instant(interval.end())),
        _ => None,
    }
}
