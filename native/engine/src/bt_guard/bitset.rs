//! 按分片下标的位集。布局与 rqbit `BitSlice<u8, Msb0>` 的原始字节一致（下标 0 = 首字节最高位），
//! 因此事件胶水可直接用 `as_raw_slice()` 构造，数据库也按同一布局存取。

/// MSB-first 位集；`len` 之外的位恒为 0。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PieceSet {
    bytes: Vec<u8>,
    len: usize,
}

impl PieceSet {
    pub fn new(len: usize) -> Self {
        Self {
            bytes: vec![0; len.div_ceil(8)],
            len,
        }
    }

    /// 由打包字节构造并裁到 `len` 位（多余字节丢弃、尾部无效位清零、不足补零）。
    pub fn from_msb_bytes(bytes: &[u8], len: usize) -> Self {
        let need = len.div_ceil(8);
        let mut out = vec![0u8; need];
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = *byte;
        }
        let mut set = Self { bytes: out, len };
        set.mask_tail();
        set
    }

    /// 由下标集合构造（测试 / 胶水便利）。越界下标忽略。
    pub fn from_indices(len: usize, indices: impl IntoIterator<Item = usize>) -> Self {
        let mut set = Self::new(len);
        for index in indices {
            if index < len {
                set.set(index);
            }
        }
        set
    }

    fn mask_tail(&mut self) {
        let rem = self.len % 8;
        if rem != 0
            && let Some(last) = self.bytes.last_mut()
        {
            *last &= 0xFFu8 << (8 - rem);
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn get(&self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        self.bytes
            .get(index / 8)
            .is_some_and(|byte| byte & (0x80 >> (index % 8)) != 0)
    }

    /// 置位；`index >= len` 时自动扩长。
    pub fn set(&mut self, index: usize) {
        if index >= self.len {
            self.len = index + 1;
            self.bytes.resize(self.len.div_ceil(8), 0);
        }
        if let Some(byte) = self.bytes.get_mut(index / 8) {
            *byte |= 0x80 >> (index % 8);
        }
    }

    pub fn count_ones(&self) -> usize {
        self.bytes.iter().map(|b| b.count_ones() as usize).sum()
    }

    /// 并集（长度取较大者）。
    pub fn union(&self, other: &Self) -> Self {
        let len = self.len.max(other.len);
        let mut bytes = vec![0u8; len.div_ceil(8)];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let a = self.bytes.get(index).copied().unwrap_or(0);
            let b = other.bytes.get(index).copied().unwrap_or(0);
            *slot = a | b;
        }
        Self { bytes, len }
    }

    /// `self` 中置位而 `other` 中未置位的位数。
    pub fn count_missing_from(&self, other: &Self) -> usize {
        self.bytes
            .iter()
            .enumerate()
            .map(|(index, a)| {
                let b = other.bytes.get(index).copied().unwrap_or(0);
                (a & !b).count_ones() as usize
            })
            .sum()
    }
}
