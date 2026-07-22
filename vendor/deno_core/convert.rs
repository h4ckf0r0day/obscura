// Copyright 2018-2025 the Deno authors. MIT license.

use crate::runtime::ops;
use deno_error::JsErrorBox;
use std::convert::Infallible;

/// A conversion from a rust value to a v8 value.
///
/// When passing data from Rust into JS, either
/// via an op or by calling a JS function directly,
/// you need to serialize the data into a native
/// V8 value. When using the [`op2`][deno_core::op2] macro, the return
/// value is converted to a `v8::Local<Value>` automatically,
/// and the strategy for conversion is controlled by attributes
/// like `#[smi]`, `#[number]`, `#[string]`. For types with support
/// built-in to the op2 macro, like primitives, strings, and buffers,
/// these attributes are sufficient and you don't need to worry about this trait.
///
/// If, however, you want to return a custom type from an op, or
/// simply want more control over the conversion process,
/// you can implement the `ToV8` trait. This allows you the
/// choose the best serialization strategy for your specific use case.
/// You can then use the `#[to_v8]` attribute to indicate
/// that the `#[op2]` macro should call your implementation for the conversion.
///
/// # Example
///
/// ```ignore
/// use deno_core::ToV8;
/// use deno_core::convert::Smi;
/// use deno_core::op2;
///
/// struct Foo(i32);
///
/// impl<'a> ToV8<'a> for Foo {
///   // This conversion can never fail, so we use `Infallible` as the error type.
///   // Any error type that implements `std::error::Error` can be used here.
///   type Error = std::convert::Infallible;
///
///   fn to_v8(self, scope: &mut v8::HandleScope<'a>) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
///     // For performance, pass this value as a `v8::Integer` (i.e. a `smi`).
///     // The `Smi` wrapper type implements this conversion for you.
///     Smi(self.0).to_v8(scope)
///   }
/// }
///
/// // using the `#[to_v8]` attribute tells the `op2` macro to call this implementation.
/// #[op2]
/// #[to_v8]
/// fn op_foo() -> Foo {
///   Foo(42)
/// }
/// ```
///
/// # Performance Notes
/// ## Structs
/// The natural representation of a struct in JS is an object with fields
/// corresponding the struct. This, however, is a performance footgun and
/// you should avoid creating and passing objects to V8 whenever possible.
/// In general, if you need to pass a compound type to JS, it is more performant to serialize
/// to a tuple (a `v8::Array`) rather than an object.
/// Object keys are V8 strings, and strings are expensive to pass to V8
/// and they have to be managed by the V8 garbage collector.
/// Tuples, on the other hand, are keyed by `smi`s, which are immediates
/// and don't require allocation or garbage collection.
pub trait ToV8<'a> {
  type Error: std::error::Error + Send + Sync + 'static;

  /// Converts the value to a V8 value.
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error>;
}

/// A conversion from a v8 value to a rust value.
///
/// When writing a op, or otherwise writing a function in Rust called
/// from JS, arguments passed from JS are represented as [`v8::Local<v8::Value>>`][deno_core::v8::Value].
/// To convert these values into custom Rust types, you can implement the [`FromV8`] trait.
///
/// Once you've implemented this trait, you can use the `#[from_v8]` attribute
/// to tell the [`op2`][deno_core::op2] macro to use your implementation to convert the argument
/// to the desired type.
///
/// # Example
///
/// ```ignore
/// use deno_core::FromV8;
/// use deno_error::JsErrorBox;
/// use deno_core::convert::Smi;
/// use deno_core::op2;
///
/// struct Foo(i32);
///
/// impl<'a> FromV8<'a> for Foo {
///   // This conversion can fail, so we use `JsErrorBox` as the error type.
///   // Any error type that implements `std::error::Error` can be used here.
///   type Error = JsErrorBox;
///
///   fn from_v8(scope: &mut v8::HandleScope<'a>, value: v8::Local<'a, v8::Value>) -> Result<Self, Self::Error> {
///     /// We expect this value to be a `v8::Integer`, so we use the [`Smi`][deno_core::convert::Smi] wrapper type to convert it.
///     Smi::from_v8(scope, value).map(|Smi(v)| Foo(v))
///   }
/// }
///
/// // using the `#[from_v8]` attribute tells the `op2` macro to call this implementation.
/// #[op2]
/// fn op_foo(#[from_v8] foo: Foo) {
///   let Foo(_) = foo;
/// }
/// ```
pub trait FromV8<'a>: Sized {
  type Error: std::error::Error + Send + Sync + 'static;

  /// Converts a V8 value to a Rust value.
  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error>;
}

// impls

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Marks a numeric type as being serialized as a v8 `smi` in a `v8::Integer`.
#[repr(transparent)]
pub struct Smi<T: SmallInt>(pub T);

/// A trait for types that can represent a JS `smi`.
pub trait SmallInt {
  const NAME: &'static str;

  #[allow(clippy::wrong_self_convention)]
  fn as_i32(self) -> i32;
  fn from_i32(value: i32) -> Self;
}

macro_rules! impl_smallint {
  (for $($t:ty),*) => {
    $(
      impl SmallInt for $t {
        const NAME: &'static str = stringify!($t);
        #[allow(clippy::wrong_self_convention)]
        #[inline(always)]
        fn as_i32(self) -> i32 {
          self as _
        }

        #[inline(always)]
        fn from_i32(value: i32) -> Self {
            value as _
        }
      }
    )*
  };
}

impl_smallint!(for u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl<'a, T: SmallInt> ToV8<'a> for Smi<T> {
  type Error = Infallible;

  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Integer::new(scope, self.0.as_i32()).into())
  }
}

impl<'a, T: SmallInt> FromV8<'a> for Smi<T> {
  type Error = JsErrorBox;

  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    let v = ops::to_i32_option(&value)
      .ok_or_else(|| JsErrorBox::type_error(format!("Expected {}", T::NAME)))?;
    Ok(Smi(T::from_i32(v)))
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Marks a numeric type as being serialized as a v8 `number` in a `v8::Number`.
#[repr(transparent)]
pub struct Number<T: Numeric>(pub T);

/// A trait for types that can represent a JS `number`.
pub trait Numeric: Sized {
  const NAME: &'static str;
  #[allow(clippy::wrong_self_convention)]
  fn as_f64(self) -> f64;
  fn from_value(value: &v8::Value) -> Option<Self>;
}

macro_rules! impl_numeric {
  ($($t:ty : $from: path ),*) => {
    $(
      impl Numeric for $t {
        const NAME: &'static str = stringify!($t);
        #[inline(always)]
        fn from_value(value: &v8::Value) -> Option<Self> {
          $from(value).map(|v| v as _)
        }

        #[allow(clippy::wrong_self_convention)]
        #[inline(always)]
        fn as_f64(self) -> f64 {
            self as _
        }
      }
    )*
  };
}

impl_numeric!(
  f32   : ops::to_f32_option,
  f64   : ops::to_f64_option,
  u32   : ops::to_u32_option,
  u64   : ops::to_u64_option,
  usize : ops::to_u64_option,
  i32   : ops::to_i32_option,
  i64   : ops::to_i64_option,
  isize : ops::to_i64_option
);

impl<'a, T: Numeric> ToV8<'a> for Number<T> {
  type Error = Infallible;
  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Number::new(scope, self.0.as_f64()).into())
  }
}

impl<'a, T: Numeric> FromV8<'a> for Number<T> {
  type Error = JsErrorBox;
  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    T::from_value(&value)
      .map(Number)
      .ok_or_else(|| JsErrorBox::type_error(format!("Expected {}", T::NAME)))
  }
}

impl<'a> ToV8<'a> for bool {
  type Error = Infallible;
  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Boolean::new(scope, self).into())
  }
}

impl<'a> FromV8<'a> for bool {
  type Error = JsErrorBox;
  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    value
      .try_cast::<v8::Boolean>()
      .map(|v| v.is_true())
      .map_err(|_| JsErrorBox::type_error("Expected boolean"))
  }
}

impl<'a> FromV8<'a> for String {
  type Error = Infallible;
  #[inline]
  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<String, Self::Error> {
    Ok(value.to_rust_string_lossy(scope))
  }
}
impl<'a> ToV8<'a> for String {
  type Error = Infallible;
  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::String::new(scope, &self).unwrap().into()) // TODO
  }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A wrapper type for `Option<T>` that (de)serializes `None` as `null`
#[repr(transparent)]
pub struct OptionNull<T>(pub Option<T>);

impl<T> From<Option<T>> for OptionNull<T> {
  fn from(option: Option<T>) -> Self {
    Self(option)
  }
}

impl<T> From<OptionNull<T>> for Option<T> {
  fn from(value: OptionNull<T>) -> Self {
    value.0
  }
}

impl<'a, T> ToV8<'a> for OptionNull<T>
where
  T: ToV8<'a>,
{
  type Error = T::Error;

  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    match self.0 {
      Some(value) => value.to_v8(scope),
      None => Ok(v8::null(scope).into()),
    }
  }
}

impl<'a, T> FromV8<'a> for OptionNull<T>
where
  T: FromV8<'a>,
{
  type Error = T::Error;

  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    if value.is_null() {
      Ok(OptionNull(None))
    } else {
      T::from_v8(scope, value).map(|v| OptionNull(Some(v)))
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A wrapper type for `Option<T>` that (de)serializes `None` as `undefined`
#[repr(transparent)]
pub struct OptionUndefined<T>(pub Option<T>);

impl<T> From<Option<T>> for OptionUndefined<T> {
  fn from(option: Option<T>) -> Self {
    Self(option)
  }
}

impl<T> From<OptionUndefined<T>> for Option<T> {
  fn from(value: OptionUndefined<T>) -> Self {
    value.0
  }
}

impl<'a, T> ToV8<'a> for OptionUndefined<T>
where
  T: ToV8<'a>,
{
  type Error = T::Error;

  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    match self.0 {
      Some(value) => value.to_v8(scope),
      None => Ok(v8::undefined(scope).into()),
    }
  }
}

impl<'a, T> FromV8<'a> for OptionUndefined<T>
where
  T: FromV8<'a>,
{
  type Error = T::Error;

  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    if value.is_undefined() {
      Ok(OptionUndefined(None))
    } else {
      T::from_v8(scope, value).map(|v| OptionUndefined(Some(v)))
    }
  }
}
