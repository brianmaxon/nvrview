//! Plain integer sizes and rectangles, laid out for a C interface.

/// A size in pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// Creates a size.
    pub const fn new(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    /// True when either dimension is zero.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A rectangle in pixels: origin at the top left, `x` to the right, `y` down.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// The empty rectangle at the origin.
    pub const ZERO: Rect = Rect::new(0, 0, 0, 0);

    /// Creates a rectangle.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The rectangle's size.
    pub const fn size(self) -> Size {
        Size::new(self.width, self.height)
    }

    /// True when either dimension is zero.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}
