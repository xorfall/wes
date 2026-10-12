// Modified by Wes for bounded screen recovery; see ../WES.md.
use crate::pen::Pen;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Cell(char, Occupancy, Pen, [char; 6], u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Occupancy {
    Single,
    WideHead,
    WideTail,
}

impl Occupancy {
    pub(crate) fn width(&self) -> u8 {
        match self {
            Occupancy::Single => 1,
            Occupancy::WideHead => 2,
            Occupancy::WideTail => 0,
        }
    }
}

impl Cell {
    pub(crate) fn new(ch: char, occupancy: Occupancy, pen: Pen) -> Self {
        Cell(ch, occupancy, pen, ['\0'; 6], 0)
    }

    pub(crate) fn blank(pen: Pen) -> Self {
        Self::new(' ', Occupancy::Single, pen)
    }

    pub fn is_default(&self) -> bool {
        self.0 == ' ' && self.1 == Occupancy::Single && self.2.is_default() && self.4 == 0
    }

    pub fn char(&self) -> char {
        self.0
    }
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.0).chain(self.3[..usize::from(self.4 & 0x7f)].iter().copied())
    }
    pub(crate) fn append_zero(&mut self, ch: char) -> bool {
        if usize::from(self.4 & 0x7f) == self.3.len() {
            self.4 |= 0x80;
            return false;
        }
        self.3[usize::from(self.4)] = ch;
        self.4 += 1;
        true
    }
    pub(crate) fn has_zero(&self) -> bool {
        self.4 != 0
    }
    pub(crate) fn has_loss(&self) -> bool {
        self.4 & 0x80 != 0
    }

    pub(crate) fn occupancy(&self) -> Occupancy {
        self.1
    }

    pub fn width(&self) -> u8 {
        self.1.width()
    }

    pub fn pen(&self) -> &Pen {
        &self.2
    }

    pub(crate) fn set(&mut self, ch: char, occupancy: Occupancy, pen: Pen) {
        self.0 = ch;
        self.1 = occupancy;
        self.2 = pen;
        self.3.fill('\0');
        self.4 = 0;
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(Pen::default())
    }
}
