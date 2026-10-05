use super::Decimal;
use bigdecimal::{BigDecimal, num_bigint::BigInt};
use std::cmp::Ordering;
use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub enum DecimalOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    RoundDivide(u32),
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum NumericError {
    #[error("numeric expansion exceeds calculation limit")]
    Limit,
    #[error("division by zero")]
    Zero,
    #[error("division is nonterminating; use roundDiv with an explicit scale")]
    Nonterminating,
}
fn pow10(n: i64, max: usize) -> Result<BigInt, NumericError> {
    if n < 0 || n as u64 > max as u64 {
        return Err(NumericError::Limit);
    }
    Ok(BigInt::from(10u8).pow(n as u32))
}
fn absolute(n: BigInt) -> BigInt {
    if n < BigInt::from(0) { -n } else { n }
}
fn gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
    a = absolute(a);
    b = absolute(b);
    while b != BigInt::from(0) {
        let rem = &a % &b;
        a = b;
        b = rem;
    }
    a
}
impl Decimal {
    pub fn calculate(
        &self,
        op: DecimalOp,
        other: &Self,
        max_digits: usize,
    ) -> Result<Self, NumericError> {
        if max_digits > 100_000
            || self.compact_text_size_bound() > max_digits as u64 + 32
            || other.compact_text_size_bound() > max_digits as u64 + 32
        {
            return Err(NumericError::Limit);
        }
        let (mut a, sa) = self.0.as_bigint_and_exponent();
        let (mut b, sb) = other.0.as_bigint_and_exponent();
        let (coefficient, scale) = match op {
            DecimalOp::Add | DecimalOp::Subtract => {
                let scale = sa.max(sb);
                a *= pow10(scale - sa, max_digits)?;
                b *= pow10(scale - sb, max_digits)?;
                (
                    if matches!(op, DecimalOp::Add) {
                        a + b
                    } else {
                        a - b
                    },
                    scale,
                )
            }
            DecimalOp::Multiply => (a * b, sa.checked_add(sb).ok_or(NumericError::Limit)?),
            DecimalOp::Divide | DecimalOp::RoundDivide(_) => {
                if b == BigInt::from(0) {
                    return Err(NumericError::Zero);
                }
                if b < BigInt::from(0) {
                    a = -a;
                    b = -b;
                }
                if let DecimalOp::RoundDivide(scale) = op {
                    if scale as usize > max_digits {
                        return Err(NumericError::Limit);
                    }
                    let shift = i64::from(scale) + sb - sa;
                    if shift >= 0 {
                        a *= pow10(shift, max_digits)?;
                    } else {
                        b *= pow10(-shift, max_digits)?;
                    }
                    let mut quotient = &a / &b;
                    let rem = absolute(&a % &b) * 2u8;
                    if rem > b || (rem == b && &quotient % 2u8 != BigInt::from(0)) {
                        quotient += if a < BigInt::from(0) { -1 } else { 1 };
                    }
                    (quotient, i64::from(scale))
                } else {
                    let common = gcd(a.clone(), b.clone());
                    a /= &common;
                    b /= &common;
                    let mut twos = 0u32;
                    let mut fives = 0u32;
                    while &b % 2u8 == BigInt::from(0) {
                        b /= 2u8;
                        twos += 1;
                        if twos as usize > max_digits * 4 {
                            return Err(NumericError::Limit);
                        }
                    }
                    while &b % 5u8 == BigInt::from(0) {
                        b /= 5u8;
                        fives += 1;
                        if fives as usize > max_digits * 4 {
                            return Err(NumericError::Limit);
                        }
                    }
                    if b != BigInt::from(1) {
                        return Err(NumericError::Nonterminating);
                    }
                    let places = twos.max(fives);
                    if places as usize > max_digits {
                        return Err(NumericError::Limit);
                    }
                    a *= BigInt::from(2u8).pow(places - twos);
                    a *= BigInt::from(5u8).pow(places - fives);
                    (a, sa - sb + i64::from(places))
                }
            }
        };
        if coefficient.to_str_radix(10).len() > max_digits || i32::try_from(scale).is_err() {
            return Err(NumericError::Limit);
        }
        Ok(Decimal(BigDecimal::new(coefficient, scale)))
    }
    pub fn compare_numeric(
        &self,
        other: &Self,
        max_digits: usize,
    ) -> Result<Ordering, NumericError> {
        let difference = self.calculate(DecimalOp::Subtract, other, max_digits)?;
        Ok(difference
            .0
            .as_bigint_and_exponent()
            .0
            .cmp(&BigInt::from(0)))
    }
}
