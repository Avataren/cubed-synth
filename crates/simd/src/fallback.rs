use std::ops::{
    Add, AddAssign, BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Div, DivAssign,
    Index, IndexMut, Mul, MulAssign, Neg, Not, Rem, RemAssign, Shl, ShlAssign, Shr, ShrAssign, Sub,
    SubAssign,
};

mod sealed {
    pub trait Sealed {}
}

/// A type a [`Simd`] lane can hold.
pub trait SimdElement: Copy + Default + PartialEq + PartialOrd + sealed::Sealed + 'static {}

macro_rules! elements {
    ($($t:ty)*) => {$(
        impl sealed::Sealed for $t {}
        impl SimdElement for $t {}
    )*};
}
elements!(f32 f64 i8 i16 i32 i64 isize u8 u16 u32 u64 usize);

/// `N` lanes of `T`.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(transparent)]
pub struct Simd<T: SimdElement, const N: usize>([T; N]);

impl<T: SimdElement, const N: usize> Default for Simd<T, N> {
    #[inline(always)]
    fn default() -> Self {
        Self([T::default(); N])
    }
}

impl<T: SimdElement, const N: usize> Simd<T, N> {
    pub const LEN: usize = N;

    #[inline(always)]
    pub const fn splat(value: T) -> Self {
        Self([value; N])
    }

    #[inline(always)]
    pub const fn from_array(array: [T; N]) -> Self {
        Self(array)
    }

    #[inline(always)]
    pub const fn to_array(self) -> [T; N] {
        self.0
    }

    #[inline(always)]
    pub const fn as_array(&self) -> &[T; N] {
        &self.0
    }

    #[inline(always)]
    pub fn as_mut_array(&mut self) -> &mut [T; N] {
        &mut self.0
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        N
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        N == 0
    }

    /// The first `N` elements of `slice`. Panics if it is shorter.
    #[inline(always)]
    #[track_caller]
    pub fn from_slice(slice: &[T]) -> Self {
        assert!(
            slice.len() >= N,
            "slice length must be at least the number of lanes"
        );
        let mut out = [T::default(); N];
        out.copy_from_slice(&slice[..N]);
        Self(out)
    }

    /// Writes the lanes to the first `N` elements of `slice`. Panics if it is shorter.
    #[inline(always)]
    #[track_caller]
    pub fn copy_to_slice(self, slice: &mut [T]) {
        assert!(
            slice.len() >= N,
            "slice length must be at least the number of lanes"
        );
        slice[..N].copy_from_slice(&self.0);
    }

    /// The first `N` elements of `slice`, `T::default()` past its end.
    #[inline(always)]
    pub fn load_or_default(slice: &[T]) -> Self {
        let mut out = [T::default(); N];
        let n = slice.len().min(N);
        out[..n].copy_from_slice(&slice[..n]);
        Self(out)
    }

    /// The lanes of `self` and `other` alternated: the first half of the
    /// result in the first vector, the second half in the second.
    #[inline(always)]
    pub fn interleave(self, other: Self) -> (Self, Self) {
        let mut lo = [T::default(); N];
        let mut hi = [T::default(); N];
        for i in 0..N {
            let (a, b) = (self.0[i], other.0[i]);
            let j = 2 * i;
            if j < N {
                lo[j] = a;
                lo[j + 1] = b;
            } else {
                hi[j - N] = a;
                hi[j - N + 1] = b;
            }
        }
        (Self(lo), Self(hi))
    }

    /// The inverse of [`Simd::interleave`].
    #[inline(always)]
    pub fn deinterleave(self, other: Self) -> (Self, Self) {
        let mut even = [T::default(); N];
        let mut odd = [T::default(); N];
        for i in 0..N {
            let (src, j) = if 2 * i < N {
                (&self.0, 2 * i)
            } else {
                (&other.0, 2 * i - N)
            };
            even[i] = src[j];
            odd[i] = src[j + 1];
        }
        (Self(even), Self(odd))
    }

    #[inline(always)]
    fn map_lanes(self, f: impl Fn(T) -> T) -> Self {
        Self(self.0.map(f))
    }

    #[inline(always)]
    fn zip_lanes(self, other: Self, f: impl Fn(T, T) -> T) -> Self {
        let mut out = self.0;
        for i in 0..N {
            out[i] = f(self.0[i], other.0[i]);
        }
        Self(out)
    }

    #[inline(always)]
    fn compare(self, other: Self, f: impl Fn(&T, &T) -> bool) -> Mask<T::Mask, N>
    where
        T: HasMask,
    {
        let mut out = [false; N];
        for i in 0..N {
            out[i] = f(&self.0[i], &other.0[i]);
        }
        Mask::from_array(out)
    }
}

impl<T: SimdElement, const N: usize> From<[T; N]> for Simd<T, N> {
    #[inline(always)]
    fn from(array: [T; N]) -> Self {
        Self(array)
    }
}

impl<T: SimdElement, const N: usize> From<Simd<T, N>> for [T; N] {
    #[inline(always)]
    fn from(vector: Simd<T, N>) -> Self {
        vector.0
    }
}

impl<T: SimdElement, const N: usize> AsRef<[T]> for Simd<T, N> {
    #[inline(always)]
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T: SimdElement, const N: usize> AsMut<[T]> for Simd<T, N> {
    #[inline(always)]
    fn as_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl<T: SimdElement, const N: usize> Index<usize> for Simd<T, N> {
    type Output = T;
    #[inline(always)]
    fn index(&self, index: usize) -> &T {
        &self.0[index]
    }
}

impl<T: SimdElement, const N: usize> IndexMut<usize> for Simd<T, N> {
    #[inline(always)]
    fn index_mut(&mut self, index: usize) -> &mut T {
        &mut self.0[index]
    }
}

/// `as` between lane types.
pub trait SimdCast<U>: SimdElement {
    fn cast(self) -> U;
}

macro_rules! casts {
    ($($from:ty)*) => {$( casts!(@to $from; f32 f64 i8 i16 i32 i64 isize u8 u16 u32 u64 usize); )*};
    (@to $from:ty; $($to:ty)*) => {$(
        impl SimdCast<$to> for $from {
            #[inline(always)]
            fn cast(self) -> $to {
                self as $to
            }
        }
    )*};
}
casts!(f32 f64 i8 i16 i32 i64 isize u8 u16 u32 u64 usize);

// --- arithmetic -----------------------------------------------------------

macro_rules! binop {
    ($trait:ident $method:ident $assign:ident $assign_method:ident; $t:ty; $f:expr) => {
        impl<const N: usize> $trait for Simd<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn $method(self, rhs: Self) -> Self {
                self.zip_lanes(rhs, $f)
            }
        }
        impl<const N: usize> $trait<$t> for Simd<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn $method(self, rhs: $t) -> Self {
                self.zip_lanes(Self::splat(rhs), $f)
            }
        }
        impl<const N: usize> $trait<Simd<$t, N>> for $t {
            type Output = Simd<$t, N>;
            #[inline(always)]
            fn $method(self, rhs: Simd<$t, N>) -> Simd<$t, N> {
                Simd::splat(self).zip_lanes(rhs, $f)
            }
        }
        impl<const N: usize> $assign for Simd<$t, N> {
            #[inline(always)]
            fn $assign_method(&mut self, rhs: Self) {
                *self = self.zip_lanes(rhs, $f);
            }
        }
        impl<const N: usize> $assign<$t> for Simd<$t, N> {
            #[inline(always)]
            fn $assign_method(&mut self, rhs: $t) {
                *self = self.zip_lanes(Self::splat(rhs), $f);
            }
        }
    };
}

macro_rules! float_ops {
    ($($t:ty)*) => {$(
        binop!(Add add AddAssign add_assign; $t; |a, b| a + b);
        binop!(Sub sub SubAssign sub_assign; $t; |a, b| a - b);
        binop!(Mul mul MulAssign mul_assign; $t; |a, b| a * b);
        binop!(Div div DivAssign div_assign; $t; |a, b| a / b);
        binop!(Rem rem RemAssign rem_assign; $t; |a, b| a % b);
        impl<const N: usize> Neg for Simd<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self {
                self.map_lanes(|a| -a)
            }
        }
    )*};
}
float_ops!(f32 f64);

macro_rules! int_ops {
    ($($t:ty)*) => {$(
        binop!(Add add AddAssign add_assign; $t; |a, b| a.wrapping_add(b));
        binop!(Sub sub SubAssign sub_assign; $t; |a, b| a.wrapping_sub(b));
        binop!(Mul mul MulAssign mul_assign; $t; |a, b| a.wrapping_mul(b));
        binop!(Div div DivAssign div_assign; $t; |a, b| a.wrapping_div(b));
        binop!(Rem rem RemAssign rem_assign; $t; |a, b| a.wrapping_rem(b));
        binop!(Shl shl ShlAssign shl_assign; $t; |a, b| a.wrapping_shl(b as u32));
        binop!(Shr shr ShrAssign shr_assign; $t; |a, b| a.wrapping_shr(b as u32));
        binop!(BitAnd bitand BitAndAssign bitand_assign; $t; |a, b| a & b);
        binop!(BitOr bitor BitOrAssign bitor_assign; $t; |a, b| a | b);
        binop!(BitXor bitxor BitXorAssign bitxor_assign; $t; |a, b| a ^ b);
        impl<const N: usize> Not for Simd<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn not(self) -> Self {
                self.map_lanes(|a| !a)
            }
        }
    )*};
}
int_ops!(i8 i16 i32 i64 isize u8 u16 u32 u64 usize);

macro_rules! signed_neg {
    ($($t:ty)*) => {$(
        impl<const N: usize> Neg for Simd<$t, N> {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self {
                self.map_lanes(|a| a.wrapping_neg())
            }
        }
    )*};
}
signed_neg!(i8 i16 i32 i64 isize);

// --- masks ----------------------------------------------------------------

/// The integer type whose masks a lane type's comparisons produce.
pub trait HasMask: SimdElement {
    type Mask: MaskElement;
}

/// A [`Mask`] element type (std's signed integers).
pub trait MaskElement: SimdElement {}
impl MaskElement for i8 {}
impl MaskElement for i16 {}
impl MaskElement for i32 {}
impl MaskElement for i64 {}
impl MaskElement for isize {}

macro_rules! has_mask {
    ($($t:ty => $m:ty),*) => {$( impl HasMask for $t { type Mask = $m; } )*};
}
has_mask!(f32 => i32, f64 => i64, i8 => i8, i16 => i16, i32 => i32, i64 => i64, isize => isize,
          u8 => i8, u16 => i16, u32 => i32, u64 => i64, usize => isize);

/// One boolean per lane, as a comparison of `Simd<_, N>` produces.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mask<T: MaskElement, const N: usize>([bool; N], std::marker::PhantomData<T>);

impl<T: MaskElement, const N: usize> Default for Mask<T, N> {
    #[inline(always)]
    fn default() -> Self {
        Self::splat(false)
    }
}

impl<T: MaskElement, const N: usize> Mask<T, N> {
    #[inline(always)]
    pub fn splat(value: bool) -> Self {
        Self([value; N], std::marker::PhantomData)
    }

    #[inline(always)]
    pub fn from_array(array: [bool; N]) -> Self {
        Self(array, std::marker::PhantomData)
    }

    #[inline(always)]
    pub fn to_array(self) -> [bool; N] {
        self.0
    }

    #[inline(always)]
    pub fn test(&self, index: usize) -> bool {
        self.0[index]
    }

    #[inline(always)]
    pub fn set(&mut self, index: usize, value: bool) {
        self.0[index] = value;
    }

    #[inline(always)]
    pub fn any(self) -> bool {
        self.0.iter().any(|&b| b)
    }

    #[inline(always)]
    pub fn all(self) -> bool {
        self.0.iter().all(|&b| b)
    }

    /// Lane `i` from `true_values` where the mask is set, from `false_values` where not.
    #[inline(always)]
    pub fn select<U: HasMask<Mask = T>>(
        self,
        true_values: Simd<U, N>,
        false_values: Simd<U, N>,
    ) -> Simd<U, N> {
        let mut out = false_values.0;
        for i in 0..N {
            if self.0[i] {
                out[i] = true_values.0[i];
            }
        }
        Simd(out)
    }
}

impl<T: MaskElement, const N: usize> BitAnd for Mask<T, N> {
    type Output = Self;
    #[inline(always)]
    fn bitand(mut self, rhs: Self) -> Self {
        for i in 0..N {
            self.0[i] &= rhs.0[i];
        }
        self
    }
}

impl<T: MaskElement, const N: usize> BitOr for Mask<T, N> {
    type Output = Self;
    #[inline(always)]
    fn bitor(mut self, rhs: Self) -> Self {
        for i in 0..N {
            self.0[i] |= rhs.0[i];
        }
        self
    }
}

impl<T: MaskElement, const N: usize> BitXor for Mask<T, N> {
    type Output = Self;
    #[inline(always)]
    fn bitxor(mut self, rhs: Self) -> Self {
        for i in 0..N {
            self.0[i] ^= rhs.0[i];
        }
        self
    }
}

impl<T: MaskElement, const N: usize> Not for Mask<T, N> {
    type Output = Self;
    #[inline(always)]
    fn not(mut self) -> Self {
        for i in 0..N {
            self.0[i] = !self.0[i];
        }
        self
    }
}

// --- traits ---------------------------------------------------------------

pub mod cmp {
    use super::{HasMask, Mask, Simd, SimdElement};

    /// Lane-wise equality.
    pub trait SimdPartialEq {
        type Mask;
        fn simd_eq(self, other: Self) -> Self::Mask;
        fn simd_ne(self, other: Self) -> Self::Mask;
    }

    /// Lane-wise ordering.
    pub trait SimdPartialOrd: SimdPartialEq {
        fn simd_lt(self, other: Self) -> Self::Mask;
        fn simd_le(self, other: Self) -> Self::Mask;
        fn simd_gt(self, other: Self) -> Self::Mask;
        fn simd_ge(self, other: Self) -> Self::Mask;
    }

    /// Lane-wise min/max/clamp for integer vectors.
    pub trait SimdOrd: SimdPartialOrd {
        fn simd_max(self, other: Self) -> Self;
        fn simd_min(self, other: Self) -> Self;
        fn simd_clamp(self, min: Self, max: Self) -> Self;
    }

    impl<T: SimdElement + HasMask, const N: usize> SimdPartialEq for Simd<T, N> {
        type Mask = Mask<T::Mask, N>;
        #[inline(always)]
        fn simd_eq(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a == b)
        }
        #[inline(always)]
        fn simd_ne(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a != b)
        }
    }

    impl<T: SimdElement + HasMask, const N: usize> SimdPartialOrd for Simd<T, N> {
        #[inline(always)]
        fn simd_lt(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a < b)
        }
        #[inline(always)]
        fn simd_le(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a <= b)
        }
        #[inline(always)]
        fn simd_gt(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a > b)
        }
        #[inline(always)]
        fn simd_ge(self, other: Self) -> Self::Mask {
            self.compare(other, |a, b| a >= b)
        }
    }

    macro_rules! ord {
        ($($t:ty)*) => {$(
            impl<const N: usize> SimdOrd for Simd<$t, N> {
                #[inline(always)]
                fn simd_max(self, other: Self) -> Self {
                    self.zip_lanes(other, Ord::max)
                }
                #[inline(always)]
                fn simd_min(self, other: Self) -> Self {
                    self.zip_lanes(other, Ord::min)
                }
                #[inline(always)]
                #[track_caller]
                fn simd_clamp(self, min: Self, max: Self) -> Self {
                    assert!(
                        min.0.iter().zip(max.0.iter()).all(|(a, b)| a <= b),
                        "each element in `min` must be less than or equal to the corresponding element in `max`",
                    );
                    self.simd_max(min).simd_min(max)
                }
            }
        )*};
    }
    ord!(i8 i16 i32 i64 isize u8 u16 u32 u64 usize);
}

pub mod num {
    use super::{Simd, SimdCast, SimdElement};

    /// Float vector operations.
    pub trait SimdFloat: Copy {
        type Scalar;
        type Cast<U: SimdElement>;
        /// Lane-wise `as` conversion.
        fn cast<U: SimdElement>(self) -> Self::Cast<U>
        where
            Self::Scalar: SimdCast<U>;
        fn abs(self) -> Self;
        fn simd_max(self, other: Self) -> Self;
        fn simd_min(self, other: Self) -> Self;
        fn simd_clamp(self, min: Self, max: Self) -> Self;
        fn reduce_sum(self) -> Self::Scalar;
        fn reduce_product(self) -> Self::Scalar;
        fn reduce_max(self) -> Self::Scalar;
        fn reduce_min(self) -> Self::Scalar;
        fn recip(self) -> Self;
        fn signum(self) -> Self;
        fn copysign(self, sign: Self) -> Self;
        fn to_degrees(self) -> Self;
        fn to_radians(self) -> Self;
    }

    /// Integer vector operations.
    pub trait SimdInt: Copy {
        type Scalar;
        type Cast<U: SimdElement>;
        /// Lane-wise `as` conversion.
        fn cast<U: SimdElement>(self) -> Self::Cast<U>
        where
            Self::Scalar: SimdCast<U>;
        fn abs(self) -> Self;
        fn saturating_add(self, other: Self) -> Self;
        fn saturating_sub(self, other: Self) -> Self;
        fn reduce_sum(self) -> Self::Scalar;
        fn reduce_product(self) -> Self::Scalar;
        fn reduce_max(self) -> Self::Scalar;
        fn reduce_min(self) -> Self::Scalar;
        fn reduce_and(self) -> Self::Scalar;
        fn reduce_or(self) -> Self::Scalar;
        fn reduce_xor(self) -> Self::Scalar;
    }

    macro_rules! float {
        ($($t:ty)*) => {$(
            impl<const N: usize> SimdFloat for Simd<$t, N> {
                type Scalar = $t;
                type Cast<U: SimdElement> = Simd<U, N>;
                #[inline(always)]
                fn cast<U: SimdElement>(self) -> Simd<U, N>
                where
                    $t: SimdCast<U>,
                {
                    Simd(self.0.map(SimdCast::cast))
                }
                #[inline(always)]
                fn abs(self) -> Self {
                    self.map_lanes(<$t>::abs)
                }
                #[inline(always)]
                fn simd_max(self, other: Self) -> Self {
                    self.zip_lanes(other, <$t>::max)
                }
                #[inline(always)]
                fn simd_min(self, other: Self) -> Self {
                    self.zip_lanes(other, <$t>::min)
                }
                #[inline(always)]
                #[track_caller]
                fn simd_clamp(self, min: Self, max: Self) -> Self {
                    assert!(
                        min.0.iter().zip(max.0.iter()).all(|(a, b)| a <= b),
                        "each element in `min` must be less than or equal to the corresponding element in `max`",
                    );
                    self.simd_max(min).simd_min(max)
                }
                #[inline(always)]
                fn reduce_sum(self) -> $t {
                    self.0.iter().fold(-0.0, |acc, &x| acc + x)
                }
                #[inline(always)]
                fn reduce_product(self) -> $t {
                    self.0.iter().fold(1.0, |acc, &x| acc * x)
                }
                #[inline(always)]
                fn reduce_max(self) -> $t {
                    self.0.iter().copied().fold(<$t>::NAN, <$t>::max)
                }
                #[inline(always)]
                fn reduce_min(self) -> $t {
                    self.0.iter().copied().fold(<$t>::NAN, <$t>::min)
                }
                #[inline(always)]
                fn recip(self) -> Self {
                    self.map_lanes(<$t>::recip)
                }
                #[inline(always)]
                fn signum(self) -> Self {
                    self.map_lanes(<$t>::signum)
                }
                #[inline(always)]
                fn copysign(self, sign: Self) -> Self {
                    self.zip_lanes(sign, <$t>::copysign)
                }
                #[inline(always)]
                fn to_degrees(self) -> Self {
                    self.map_lanes(<$t>::to_degrees)
                }
                #[inline(always)]
                fn to_radians(self) -> Self {
                    self.map_lanes(<$t>::to_radians)
                }
            }
        )*};
    }
    float!(f32 f64);

    macro_rules! int {
        ($($t:ty)*) => {$(
            impl<const N: usize> SimdInt for Simd<$t, N> {
                type Scalar = $t;
                type Cast<U: SimdElement> = Simd<U, N>;
                #[inline(always)]
                fn cast<U: SimdElement>(self) -> Simd<U, N>
                where
                    $t: SimdCast<U>,
                {
                    Simd(self.0.map(SimdCast::cast))
                }
                #[inline(always)]
                fn abs(self) -> Self {
                    self.map_lanes(<$t>::wrapping_abs)
                }
                #[inline(always)]
                fn saturating_add(self, other: Self) -> Self {
                    self.zip_lanes(other, <$t>::saturating_add)
                }
                #[inline(always)]
                fn saturating_sub(self, other: Self) -> Self {
                    self.zip_lanes(other, <$t>::saturating_sub)
                }
                #[inline(always)]
                fn reduce_sum(self) -> $t {
                    self.0.iter().fold(0, |acc: $t, &x| acc.wrapping_add(x))
                }
                #[inline(always)]
                fn reduce_product(self) -> $t {
                    self.0.iter().fold(1, |acc: $t, &x| acc.wrapping_mul(x))
                }
                #[inline(always)]
                fn reduce_max(self) -> $t {
                    self.0.iter().copied().fold(<$t>::MIN, Ord::max)
                }
                #[inline(always)]
                fn reduce_min(self) -> $t {
                    self.0.iter().copied().fold(<$t>::MAX, Ord::min)
                }
                #[inline(always)]
                fn reduce_and(self) -> $t {
                    self.0.iter().fold(!0, |acc, &x| acc & x)
                }
                #[inline(always)]
                fn reduce_or(self) -> $t {
                    self.0.iter().fold(0, |acc, &x| acc | x)
                }
                #[inline(always)]
                fn reduce_xor(self) -> $t {
                    self.0.iter().fold(0, |acc, &x| acc ^ x)
                }
            }
        )*};
    }
    int!(i8 i16 i32 i64 isize);
}

/// Float functions std provides lane-wise (`std::simd::StdFloat`).
pub trait StdFloat: Copy {
    fn mul_add(self, a: Self, b: Self) -> Self;
    fn sqrt(self) -> Self;
    fn floor(self) -> Self;
    fn ceil(self) -> Self;
    fn round(self) -> Self;
    fn trunc(self) -> Self;
    fn fract(self) -> Self;
    fn sin(self) -> Self;
    fn cos(self) -> Self;
    fn exp(self) -> Self;
    fn exp2(self) -> Self;
    fn ln(self) -> Self;
    fn log2(self) -> Self;
    fn log10(self) -> Self;
}

macro_rules! std_float {
    ($($t:ty)*) => {$(
        impl<const N: usize> StdFloat for Simd<$t, N> {
            #[inline(always)]
            fn mul_add(self, a: Self, b: Self) -> Self {
                let mut out = self.0;
                for i in 0..N {
                    out[i] = self.0[i].mul_add(a.0[i], b.0[i]);
                }
                Simd(out)
            }
            #[inline(always)]
            fn sqrt(self) -> Self { self.map_lanes(<$t>::sqrt) }
            #[inline(always)]
            fn floor(self) -> Self { self.map_lanes(<$t>::floor) }
            #[inline(always)]
            fn ceil(self) -> Self { self.map_lanes(<$t>::ceil) }
            #[inline(always)]
            fn round(self) -> Self { self.map_lanes(<$t>::round) }
            #[inline(always)]
            fn trunc(self) -> Self { self.map_lanes(<$t>::trunc) }
            #[inline(always)]
            fn fract(self) -> Self { self.map_lanes(<$t>::fract) }
            #[inline(always)]
            fn sin(self) -> Self { self.map_lanes(<$t>::sin) }
            #[inline(always)]
            fn cos(self) -> Self { self.map_lanes(<$t>::cos) }
            #[inline(always)]
            fn exp(self) -> Self { self.map_lanes(<$t>::exp) }
            #[inline(always)]
            fn exp2(self) -> Self { self.map_lanes(<$t>::exp2) }
            #[inline(always)]
            fn ln(self) -> Self { self.map_lanes(<$t>::ln) }
            #[inline(always)]
            fn log2(self) -> Self { self.map_lanes(<$t>::log2) }
            #[inline(always)]
            fn log10(self) -> Self { self.map_lanes(<$t>::log10) }
        }
    )*};
}
std_float!(f32 f64);

// --- aliases --------------------------------------------------------------

macro_rules! aliases {
    ($($name:ident = $t:ty, $n:literal;)*) => {$( #[allow(non_camel_case_types)] pub type $name = Simd<$t, $n>; )*};
}
aliases! {
    f32x2 = f32, 2; f32x4 = f32, 4; f32x8 = f32, 8; f32x16 = f32, 16;
    f64x2 = f64, 2; f64x4 = f64, 4; f64x8 = f64, 8;
    i8x16 = i8, 16; i16x4 = i16, 4; i16x8 = i16, 8;
    i32x2 = i32, 2; i32x4 = i32, 4; i32x8 = i32, 8;
    i64x2 = i64, 2; i64x4 = i64, 4;
    u8x16 = u8, 16; u16x8 = u16, 8; u32x4 = u32, 4; u32x8 = u32, 8; u64x2 = u64, 2;
}

#[allow(non_camel_case_types)]
pub type mask32x4 = Mask<i32, 4>;
#[allow(non_camel_case_types)]
pub type mask64x4 = Mask<i64, 4>;

pub mod prelude {
    pub use super::cmp::{SimdOrd, SimdPartialEq, SimdPartialOrd};
    pub use super::num::{SimdFloat, SimdInt};
    pub use super::{
        f32x2, f32x4, f32x8, f64x2, f64x4, i16x4, i16x8, i32x2, i32x4, i32x8, i64x2, i64x4,
        mask32x4, mask64x4, u32x4, Mask, Simd, StdFloat,
    };
}

#[cfg(test)]
mod tests {
    use super::cmp::{SimdOrd, SimdPartialOrd};
    use super::num::{SimdFloat, SimdInt};
    use super::*;

    #[test]
    fn integer_arithmetic_and_shifts_wrap() {
        let a = i32x4::splat(i32::MAX) + i32x4::splat(1);
        assert_eq!(a.to_array(), [i32::MIN; 4]);
        let s = i32x4::from_array([-8, 8, 1, 1]) >> i32x4::splat(1);
        assert_eq!(s.to_array(), [-4, 4, 0, 0]);
        assert_eq!((i32x4::splat(1) << i32x4::splat(33)).to_array(), [2; 4]);
    }

    #[test]
    fn casts_truncate_like_as() {
        let v = i32x4::from_array([0x12345, -1, 0x7fff, -0x8000]).cast::<i16>();
        assert_eq!(v.to_array(), [0x2345, -1, 0x7fff, -0x8000]);
        let w = i32x4::splat(-3).cast::<i64>();
        assert_eq!(w.to_array(), [-3i64; 4]);
    }

    #[test]
    fn interleave_alternates_lanes() {
        let a = i16x4::from_array([0, 1, 2, 3]);
        let b = i16x4::from_array([10, 11, 12, 13]);
        let (lo, hi) = a.interleave(b);
        assert_eq!(lo.to_array(), [0, 10, 1, 11]);
        assert_eq!(hi.to_array(), [2, 12, 3, 13]);
        assert_eq!(lo.deinterleave(hi), (a, b));
    }

    #[test]
    fn clamp_select_and_reduce() {
        let v = f32x4::from_array([-2.0, 0.5, 3.0, 1.0]);
        let c = v.simd_clamp(f32x4::splat(0.0), f32x4::splat(1.0));
        assert_eq!(c.to_array(), [0.0, 0.5, 1.0, 1.0]);
        assert_eq!(v.reduce_sum(), 2.5);
        let m = v.simd_lt(f32x4::splat(1.0));
        assert_eq!(
            m.select(f32x4::splat(9.0), v).to_array(),
            [9.0, 9.0, 3.0, 1.0]
        );
        let i = i32x4::from_array([-5, 5, 0, 99]).simd_clamp(i32x4::splat(-1), i32x4::splat(1));
        assert_eq!(i.to_array(), [-1, 1, 0, 1]);
    }
}
