use crate::Rectangle;
use std::fmt;

impl Rectangle {
    pub fn with_coords(x1: f64, y1: f64, x2: f64, y2: f64) -> Rectangle {
        let mut rect = Rectangle::new();

        rect.inner.x1 = x1;
        rect.inner.y1 = y1;
        rect.inner.x2 = x2;
        rect.inner.y2 = y2;

        rect
    }

    #[inline]
    pub fn x1(&self) -> f64 {
        self.inner.x1
    }

    #[inline]
    pub fn x2(&self) -> f64 {
        self.inner.x2
    }

    #[inline]
    pub fn y1(&self) -> f64 {
        self.inner.y1
    }

    #[inline]
    pub fn y2(&self) -> f64 {
        self.inner.y2
    }
}

impl fmt::Debug for Rectangle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Rectangle")
            .field("x1", &self.x1())
            .field("x2", &self.x2())
            .field("y1", &self.y1())
            .field("y2", &self.y2())
            .finish()
    }
}
