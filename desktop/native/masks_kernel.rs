//! One integer program for both GPU drivers. Each Word denotes a device i32,
//! never a host pixel. Only declarations, instructions and addressing differ.
use std::{
    cell::{Cell, RefCell},
    fmt::{self, Display, Write},
    ops,
};

#[derive(Clone, Copy)]
pub(super) enum Target {
    Cuda,
    OpenCl,
}

struct Kernel {
    target: Target,
    next: Cell<u32>,
    body: RefCell<String>,
}

#[derive(Clone, Copy)]
struct Word<'a> {
    kernel: &'a Kernel,
    id: u32,
}

impl Display for Word<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.id)
    }
}

impl Kernel {
    fn line(&self, statement: fmt::Arguments<'_>) {
        writeln!(&mut self.body.borrow_mut(), "    {statement}").unwrap();
    }

    fn result(&self, cl: fmt::Arguments<'_>, ptx: impl FnOnce(Word<'_>)) -> Word<'_> {
        let id = self.next.get();
        self.next.set(id + 1);
        let dst = Word { kernel: self, id };
        match self.target {
            Target::OpenCl => self.line(format_args!("int {dst} = {cl};")),
            Target::Cuda => ptx(dst),
        }
        dst
    }

    fn arg(&self, name: &str) -> Word<'_> {
        self.result(format_args!("{name}"), |v| {
            self.line(format_args!("ld.param.s32 {v}, [{name}];"))
        })
    }

    fn index(&self) -> Word<'_> {
        self.result(format_args!("get_global_id(0)"), |v| {
            self.line(format_args!("mov.u32 {v}, %ctaid.x;"));
            self.line(format_args!("mov.u32 lane, %ntid.x;"));
            self.line(format_args!("mov.u32 tid, %tid.x;"));
            self.line(format_args!("mad.lo.s32 {v}, {v}, lane, tid;"));
        })
    }

    fn address(&self, name: &str, index: impl Display, stride: i32) {
        self.line(format_args!("mul.wide.s32 addr, {index}, {stride};"));
        self.line(format_args!("add.s64 addr, ptr_{name}, addr;"));
    }

    fn load(&self, name: &str, index: impl Display) -> Word<'_> {
        self.result(format_args!("{name}[{index}]"), |v| {
            let (stride, ty) = if name == "table" {
                (4, "s32")
            } else {
                (1, "u8")
            };
            self.address(name, &index, stride);
            self.line(format_args!("ld.global.{ty} {v}, [addr];"));
        })
    }

    fn store(&self, index: Word<'_>, value: impl Display) {
        match self.target {
            Target::OpenCl => self.line(format_args!("out[{index}] = {value};")),
            Target::Cuda => {
                self.address("out", index, 1);
                self.line(format_args!("st.global.u8 [addr], {value};"));
            }
        }
    }

    // A conditional forward branch. All target labels are internal constants.
    fn branch(&self, condition: Word<'_>, label: &str) {
        match self.target {
            Target::OpenCl => self.line(format_args!("if ({condition}) goto {label};")),
            Target::Cuda => self.line(format_args!(
                "setp.ne.s32 pred, {condition}, 0; @pred bra {label};"
            )),
        }
    }
}

impl<'a> Word<'a> {
    fn op(self, rhs: impl Display, symbol: &str, instruction: &str) -> Self {
        self.kernel
            .result(format_args!("{self} {symbol} {rhs}"), |v| {
                self.kernel
                    .line(format_args!("{instruction} {v}, {self}, {rhs};"))
            })
    }
    fn compare(self, rhs: impl Display, symbol: &str, test: &str) -> Self {
        self.kernel
            .result(format_args!("{self} {symbol} {rhs}"), |v| {
                self.kernel
                    .line(format_args!("setp.{test}.s32 pred, {self}, {rhs};"));
                self.kernel.line(format_args!("selp.s32 {v}, 1, 0, pred;"));
            })
    }
    fn gt(self, rhs: impl Display) -> Self {
        self.compare(rhs, ">", "gt")
    }
    fn lt(self, rhs: impl Display) -> Self {
        self.compare(rhs, "<", "lt")
    }
    fn eq(self, rhs: impl Display) -> Self {
        self.compare(rhs, "==", "eq")
    }
    fn ge(self, rhs: impl Display) -> Self {
        self.compare(rhs, ">=", "ge")
    }
    fn le(self, rhs: impl Display) -> Self {
        self.compare(rhs, "<=", "le")
    }

    fn select(self, yes: impl Display, no: impl Display) -> Self {
        self.kernel
            .result(format_args!("{self} ? {yes} : {no}"), |v| {
                self.kernel
                    .line(format_args!("setp.ne.s32 pred, {self}, 0;"));
                self.kernel
                    .line(format_args!("selp.s32 {v}, {yes}, {no}, pred;"));
            })
    }
    fn min(self, rhs: Self) -> Self {
        self.builtin(rhs, "min")
    }
    fn max(self, rhs: Self) -> Self {
        self.builtin(rhs, "max")
    }
    fn builtin(self, rhs: Self, name: &str) -> Self {
        self.kernel
            .result(format_args!("{name}({self}, {rhs})"), |v| {
                self.kernel
                    .line(format_args!("{name}.s32 {v}, {self}, {rhs};"))
            })
    }
    fn abs(self) -> Self {
        self.kernel.result(format_args!("abs({self})"), |v| {
            self.kernel.line(format_args!("abs.s32 {v}, {self};"))
        })
    }
}

macro_rules! binary {
    ($trait:ident, $method:ident, $cl:literal, $ptx:literal) => {
        impl<R: Display> ops::$trait<R> for Word<'_> {
            type Output = Self;
            fn $method(self, rhs: R) -> Self {
                self.op(rhs, $cl, $ptx)
            }
        }
    };
}
binary!(Add, add, "+", "add.s32");
binary!(Sub, sub, "-", "sub.s32");
binary!(Mul, mul, "*", "mul.lo.s32");
binary!(Div, div, "/", "div.s32");
binary!(Shr, shr, ">>", "shr.s32");
binary!(BitAnd, bitand, "&", "and.b32");
binary!(BitOr, bitor, "|", "or.b32");

// OpenCV RGB2HSV_b fixed-point conversion (see THIRD_PARTY_NOTICES.md).
fn hsv<'a>(k: &'a Kernel, [b, g, r]: [Word<'a>; 3]) -> [Word<'a>; 3] {
    let v = r.max(g).max(b);
    let d = v - r.min(g).min(b);
    let hue = v
        .eq(r)
        .select(g - b, v.eq(g).select(b - r + d * 2, r - g + d * 4));
    let h = (hue * k.load("table", d + 256) + 2048) >> 12;
    [
        h.lt(0).select(h + 180, h),
        (d * k.load("table", v) + 2048) >> 12,
        v,
    ]
}

fn pixel_program(k: &Kernel) {
    let i = k.index();
    let n = k.arg("n");
    k.branch(i.ge(n), "done");
    let row = i / k.arg("width");
    let block_index = n + i;
    k.branch(row.lt(k.arg("top")) | row.ge(k.arg("bottom")), "outside");

    let pixel_start = i * 3;
    let bg_start = row * 3;
    let pixel = [0, 1, 2].map(|c| k.load("bgr", pixel_start + c));
    let bg = [0, 1, 2].map(|c| k.load("bg", bg_start + c));
    let [h, s, v] = hsv(k, pixel);
    let [bh, bs, bv] = hsv(k, bg);
    let [db, dg, dr] = std::array::from_fn(|c| (pixel[c] - bg[c]).abs());
    let diff = db + dg + dr;
    let t = |index: i32| k.load("table", 512 + index);
    let piece = ((s.gt(t(9)) & v.lt(t(10))) | (v.lt(110) & s.gt(25))) & h.ge(t(11)) & h.le(t(12));
    let block = (bv - v).gt(t(1))
        | ((h - bh).abs().gt(t(2)) & s.gt(t(3)))
        | (s - bs).gt(t(4))
        | (bs - s).gt(t(5))
        | (s.lt(t(6)) & v.gt(t(7)))
        | diff.gt(t(0))
        | (t(13) & (v - bv).gt(t(8)) & s.le(bs));
    k.store(i, piece.select(255, 0));
    k.store(block_index, block.select(255, 0));
    k.line(format_args!(
        "{}",
        match k.target {
            Target::Cuda => "ret;",
            Target::OpenCl => "return;",
        }
    ));
    k.line(format_args!("outside:"));
    k.store(i, 0);
    k.store(block_index, 0);
    k.line(format_args!("done:"));
}

pub(super) fn generate(target: Target) -> String {
    let k = Kernel {
        target,
        next: Cell::new(0),
        body: RefCell::new(String::new()),
    };
    pixel_program(&k);
    let body = k.body.into_inner();
    match target {
        Target::OpenCl => format!(
            "__kernel void jumpbot_masks(__global const uchar *bgr, __global const uchar *bg, \
             __constant const int *table, __global uchar *out, int n, int width, int top, int bottom) {{\n{body}    return;\n}}\n"),
        Target::Cuda => {
            let mut source = format!(".version 6.0\n.target sm_52\n.address_size 64\n\n\
                .visible .entry jumpbot_masks(.param .u64 bgr, .param .u64 bg, .param .u64 table, .param .u64 out,\n\
                .param .s32 n, .param .s32 width, .param .s32 top, .param .s32 bottom) {{\n\
                .reg .pred pred; .reg .b32 v<{}>, tid, lane; .reg .b64 addr;\n", k.next.get());
            for name in ["bgr","bg","table","out"] {
                writeln!(source,".reg .b64 ptr_{name}; ld.param.u64 ptr_{name}, [{name}];").unwrap();
                writeln!(source,"cvta.to.global.u64 ptr_{name}, ptr_{name};").unwrap();
            }
            source.push_str(&body);
            source.push_str("    ret;\n}\n");
            source
        }
    }
}
