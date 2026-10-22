"""Scientific calculator engine, modelled on the Casio fx-87DE X ClassWiz.

Exact results like the ClassWiz's natural display (fractions, roots, pi forms)
via SymPy, with S<=>D decimal results alongside.

Modes: Calculate (incl. integrals, derivatives, sums, products, SOLVE, variables,
Ans/PreAns, M+/M-, DMS, Pol/Rec, GCD/LCM, remainder, prime factors), Complex,
Base-N, Statistics (1-var, 7 regressions), Distribution, Equation (polynomial,
simultaneous, inequality), Matrix, Vector, Table, Ratio, constants, conversions.
"""
import math
import random
import re
import statistics
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from fractions import Fraction

import sympy as sp
from sympy.matrices.exceptions import NonSquareMatrixError, ShapeError
from sympy.parsing.sympy_parser import (convert_xor, factorial_notation, implicit_application,
                                        implicit_multiplication, parse_expr, rationalize,
                                        standard_transformations)

X, Y = sp.Symbol("x"), sp.Symbol("y")
TRANSFORMS = standard_transformations + (implicit_multiplication, implicit_application,
                                         convert_xor, factorial_notation, rationalize)
TIME_LIMIT = 10
_pool = ThreadPoolExecutor(max_workers=2)


class CalcError(Exception):
    """Shown to the user like the ClassWiz's error screens."""


# ---------------------------------------------------------------- number formatting

SUPERSCRIPT = str.maketrans("0123456789-", "⁰¹²³⁴⁵⁶⁷⁸⁹⁻")


def _sci(x, digits):
    mant, exp = f"{x:.{digits - 1}e}".split("e")
    mant = mant.rstrip("0").rstrip(".") if "." in mant else mant
    return f"{mant}×10{str(int(exp)).translate(SUPERSCRIPT)}"


def format_number(x, fmt="norm1", digits=10):
    """Like the ClassWiz: Norm 1/Norm 2 (10 digits), Fix n (n decimals), Sci n (n significant)."""
    x = float(x)
    if math.isnan(x) or math.isinf(x):
        raise CalcError("Math ERROR")
    if x == 0:
        return "0"
    if fmt == "fix":
        if abs(x) >= 1e10:
            return _sci(x, 10)
        return f"{x:.{digits}f}"
    if fmt == "sci":
        return _sci(x, max(1, digits))
    low = 1e-2 if fmt == "norm1" else 1e-9
    if low <= abs(x) < 1e10:
        text = f"{x:.10g}"
        if "e" in text:  # .10g can still choose exponent form near the edges
            return _sci(x, 10)
        return text
    return _sci(x, 10)


def format_eng(x):
    """Engineering notation: exponent a multiple of 3."""
    x = float(x)
    if x == 0:
        return "0"
    exp = int(math.floor(math.log10(abs(x)) / 3) * 3)
    mant = x / 10 ** exp
    if exp == 0:
        return f"{mant:.10g}"
    return f"{mant:.10g}×10{str(exp).translate(SUPERSCRIPT)}"


def to_dms(deg):
    """Decimal degrees as 12°30′15.5″."""
    deg = float(deg)
    sign = "-" if deg < 0 else ""
    deg = abs(deg)
    d = int(deg)
    m = int((deg - d) * 60)
    s = round(((deg - d) * 60 - m) * 60, 2)
    if s >= 60:
        s, m = 0, m + 1
    if m >= 60:
        m, d = 0, d + 1
    s_text = f"{s:g}"
    return f"{sign}{d}°{m}′{s_text}″"


def mathml(expr):
    return f'<math display="block">{sp.mathml(expr, printer="presentation")}</math>'


# ---------------------------------------------------------------- constants & conversions

CONSTANTS = [  # CODATA 2018, the set a ClassWiz offers
    ("Universal", "c", "speed of light in vacuum", 299792458, "m/s"),
    ("Universal", "h", "Planck constant", 6.62607015e-34, "J·s"),
    ("Universal", "ħ", "reduced Planck constant", 1.054571817e-34, "J·s"),
    ("Universal", "G", "Newtonian constant of gravitation", 6.67430e-11, "m³/(kg·s²)"),
    ("Universal", "μ₀", "magnetic constant", 1.25663706212e-6, "N/A²"),
    ("Universal", "ε₀", "electric constant", 8.8541878128e-12, "F/m"),
    ("Universal", "Z₀", "characteristic impedance of vacuum", 376.730313668, "Ω"),
    ("Universal", "mₚ", "proton mass", 1.67262192369e-27, "kg"),
    ("Universal", "mₙ", "neutron mass", 1.67492749804e-27, "kg"),
    ("Universal", "mₑ", "electron mass", 9.1093837015e-31, "kg"),
    ("Universal", "mμ", "muon mass", 1.883531627e-28, "kg"),
    ("Electromagnetic", "e", "elementary charge", 1.602176634e-19, "C"),
    ("Electromagnetic", "Φ₀", "magnetic flux quantum", 2.067833848e-15, "Wb"),
    ("Electromagnetic", "G₀", "conductance quantum", 7.748091729e-5, "S"),
    ("Electromagnetic", "μB", "Bohr magneton", 9.2740100783e-24, "J/T"),
    ("Electromagnetic", "μN", "nuclear magneton", 5.0507837461e-27, "J/T"),
    ("Electromagnetic", "μₑ", "electron magnetic moment", -9.2847647043e-24, "J/T"),
    ("Electromagnetic", "μₚ", "proton magnetic moment", 1.41060679736e-26, "J/T"),
    ("Electromagnetic", "μₙ", "neutron magnetic moment", -9.6623651e-27, "J/T"),
    ("Atomic & nuclear", "a₀", "Bohr radius", 5.29177210903e-11, "m"),
    ("Atomic & nuclear", "α", "fine-structure constant", 7.2973525693e-3, ""),
    ("Atomic & nuclear", "rₑ", "classical electron radius", 2.8179403262e-15, "m"),
    ("Atomic & nuclear", "λc", "Compton wavelength", 2.42631023867e-12, "m"),
    ("Atomic & nuclear", "R∞", "Rydberg constant", 10973731.568160, "1/m"),
    ("Atomic & nuclear", "γₚ", "proton gyromagnetic ratio", 2.6752218744e8, "1/(s·T)"),
    ("Physico-chemical", "u", "atomic mass constant", 1.66053906660e-27, "kg"),
    ("Physico-chemical", "Nᴀ", "Avogadro constant", 6.02214076e23, "1/mol"),
    ("Physico-chemical", "k", "Boltzmann constant", 1.380649e-23, "J/K"),
    ("Physico-chemical", "R", "molar gas constant", 8.314462618, "J/(mol·K)"),
    ("Physico-chemical", "F", "Faraday constant", 96485.33212, "C/mol"),
    ("Physico-chemical", "σ", "Stefan–Boltzmann constant", 5.670374419e-8, "W/(m²·K⁴)"),
    ("Physico-chemical", "Vm", "molar volume of ideal gas (273.15 K, 100 kPa)", 22.71095464e-3, "m³/mol"),
    ("Adopted values", "g", "standard acceleration of gravity", 9.80665, "m/s²"),
    ("Adopted values", "atm", "standard atmosphere", 101325, "Pa"),
    ("Adopted values", "t", "Celsius temperature (0 °C in K)", 273.15, "K"),
]

# (id, from, to, factor) or (id, from, to, function, inverse) for temperatures
CONVERSIONS = [
    ("in_cm", "in", "cm", 2.54), ("ft_m", "ft", "m", 0.3048), ("yd_m", "yd", "m", 0.9144),
    ("mile_km", "mile", "km", 1.609344), ("nmile_m", "n mile", "m", 1852), ("pc_km", "pc", "km", 3.0856775814913673e13),
    ("acre_m2", "acre", "m²", 4046.8564224), ("galUS_L", "gal (US)", "L", 3.785411784), ("galUK_L", "gal (UK)", "L", 4.54609),
    ("kmh_ms", "km/h", "m/s", 1 / 3.6), ("oz_g", "oz", "g", 28.349523125), ("lb_kg", "lb", "kg", 0.45359237),
    ("atm_Pa", "atm", "Pa", 101325), ("mmHg_Pa", "mmHg", "Pa", 133.322387415), ("hp_kW", "hp", "kW", 0.745699872),
    ("kgfcm2_Pa", "kgf/cm²", "Pa", 98066.5), ("kgfm_J", "kgf·m", "J", 9.80665), ("lbfin2_kPa", "lbf/in²", "kPa", 6.894757293168),
    ("J_cal", "J", "cal", 1 / 4.184), ("kWh_MJ", "kWh", "MJ", 3.6),
    ("F_C", "°F", "°C", None),
]


def convert_unit(value, conv_id, reverse=False):
    for cid, a, b, factor in CONVERSIONS:
        if cid == conv_id:
            v = float(value)
            if factor is None:  # °F <-> °C
                result = (v - 32) * 5 / 9 if not reverse else v * 9 / 5 + 32
            else:
                result = v * factor if not reverse else v / factor
            return {"value": format_number(result), "from": b if reverse else a, "to": a if reverse else b}
    raise CalcError("Unknown conversion")


# ---------------------------------------------------------------- the calculator

class Calculator:
    def __init__(self):
        self.angle = "deg"          # deg | rad | gra
        self.fmt = "norm1"          # norm1 | norm2 | fix | sci
        self.digits = 10            # for fix/sci
        self.vars = {n: sp.Integer(0) for n in ("A", "B", "C", "D", "E", "F", "M", "x", "y")}
        self.ans = sp.Integer(0)
        self.preans = sp.Integer(0)
        self.mats = {f"Mat{n}": None for n in "ABCD"}
        self.mats["MatAns"] = None
        self.vcts = {f"Vct{n}": None for n in "ABCD"}
        self.vcts["VctAns"] = None

    # ---- settings
    def set_mode(self, angle=None, fmt=None, digits=None):
        if angle in ("deg", "rad", "gra"):
            self.angle = angle
        if fmt in ("norm1", "norm2", "fix", "sci"):
            self.fmt = fmt
        if digits is not None:
            self.digits = max(0, min(9, int(digits))) if self.fmt == "fix" else max(1, min(10, int(digits)))
        return self.memory()

    def memory(self):
        return {"angle": self.angle, "fmt": self.fmt, "digits": self.digits,
                "vars": {k: self._short(v) for k, v in self.vars.items()},
                "ans": self._short(self.ans)}

    def _short(self, v):
        try:
            return format_number(sp.N(v), "norm1")
        except Exception:
            return str(v)

    def _num(self, v):
        return format_number(sp.N(v, 15), self.fmt, self.digits)

    # ---- angles
    def _to_rad(self, a):
        return {"deg": a * sp.pi / 180, "rad": a, "gra": a * sp.pi / 200}[self.angle]

    def _from_rad(self, r):
        return {"deg": r * 180 / sp.pi, "rad": r, "gra": r * 200 / sp.pi}[self.angle]

    # ---- parsing
    def _prepare(self, text):
        s = text.strip()
        if not s:
            raise CalcError("Syntax ERROR")
        # Block anything that isn't maths: attribute access, quotes, brackets, dunders.
        if re.search(r"__|[\"'`\[\]{};\\@$&|]|lambda|import", s) or re.search(r"[A-Za-z_)]\s*\.", s):
            raise CalcError("Syntax ERROR")
        rep = [("×", "*"), ("÷", "/"), ("−", "-"), ("·", "*"), ("π", "pi"), ("ᴇ", "*10**"),
               ("⁻¹", "**(-1)"), ("²", "**2"), ("³", "**3"), ("√", "sqrt"), ("∛", "cbrt"),
               ("Ran#", "RanNum()"), ("∫", "Integ"), ("Σ", "Sigma"), ("Π", "Prod"), ("d/dx", "Deriv"),
               ("PreAns", "PREANS"), ("∠", "@ANGLE@")]
        for a, b in rep:
            s = s.replace(a, b)
        # degrees-minutes-seconds: 12°30′15″  ->  value in degrees, then into the current unit
        def dms(m):
            d, mi, se = m.group(1), m.group(2) or "0", m.group(3) or "0"
            return f"(DEGIN(({d})+({mi})/60+({se})/3600))"
        s = re.sub(r"(\d+(?:\.\d+)?)°(?:(\d+(?:\.\d+)?)′)?(?:(\d+(?:\.\d+)?)″)?", dms, s)
        s = re.sub(r"(\d+(?:\.\d+)?|\))%", r"(\1/100)", s)        # 15% -> 0.15
        if "@ANGLE@" in s:                                           # r∠θ  -> polar complex
            left, right = s.split("@ANGLE@", 1)
            s = f"POLAR(({left}),({right}))"
        return s

    def _locals(self, symbolic_xy=False):
        t, f = self._to_rad, self._from_rad
        L = {
            "pi": sp.pi, "e": sp.E, "i": sp.I, "Ans": self.ans, "PREANS": self.preans,
            "sin": lambda a: sp.sin(t(a)), "cos": lambda a: sp.cos(t(a)), "tan": lambda a: self._tan(a),
            "asin": lambda v: f(sp.asin(v)), "acos": lambda v: f(sp.acos(v)), "atan": lambda v: f(sp.atan(v)),
            "sinh": sp.sinh, "cosh": sp.cosh, "tanh": sp.tanh,
            "asinh": sp.asinh, "acosh": sp.acosh, "atanh": sp.atanh,
            "log": lambda *a: sp.log(a[0], 10) if len(a) == 1 else sp.log(a[1], a[0]),
            "ln": sp.log, "exp": sp.exp, "sqrt": sp.sqrt, "cbrt": lambda v: sp.real_root(v, 3),
            "root": lambda n, v: sp.real_root(v, n) if (sp.Integer(n).is_odd if sp.sympify(n).is_integer else False) else v ** (1 / sp.sympify(n)),
            "abs": sp.Abs, "Abs": sp.Abs,
            "nPr": lambda n, r: sp.factorial(n) / sp.factorial(n - r), "nCr": sp.binomial,
            "GCD": lambda a, b: sp.gcd(sp.Integer(a), sp.Integer(b)), "LCM": lambda a, b: sp.lcm(sp.Integer(a), sp.Integer(b)),
            "Int": lambda v: sp.sign(v) * sp.floor(sp.Abs(v)), "Intg": sp.floor,
            "Rnd": self._rnd, "RanNum": lambda: sp.Rational(random.randint(0, 999), 1000),
            "RanInt": lambda a, b: sp.Integer(random.randint(int(a), int(b))),
            "Pol": lambda a, b: ("Pol", a, b), "Rec": lambda a, b: ("Rec", a, b),
            "Rem": lambda a, b: ("Rem", a, b), "FACT": lambda n: ("FACT", n),
            "Integ": self._integral, "Deriv": self._derivative, "Sigma": self._sigma, "Prod": self._prod,
            "DEGIN": lambda d: {"deg": d, "rad": d * sp.pi / 180, "gra": d * sp.Rational(200, 180)}[self.angle],
            "POLAR": lambda r, th: r * (sp.cos(t(th)) + sp.I * sp.sin(t(th))),
            "arg": lambda z: f(sp.arg(z)), "conjg": sp.conjugate, "ReP": sp.re, "ImP": sp.im,
        }
        for k, v in self.vars.items():
            L[k] = v
        if symbolic_xy:
            L["x"], L["y"] = X, Y
        return L

    def _tan(self, a):
        r = self._to_rad(a)
        if sp.simplify(sp.cos(r)) == 0:
            raise CalcError("Math ERROR")
        return sp.tan(r)

    def _rnd(self, v):
        n = float(sp.N(v, 15))
        if self.fmt == "fix":
            return sp.nsimplify(round(n, self.digits), rational=True)
        digits = self.digits if self.fmt == "sci" else 10
        return sp.nsimplify(float(f"{n:.{digits - 1}e}"), rational=True)

    def _integral(self, fx, a, b):
        r = sp.integrate(fx, (X, a, b))
        if r.has(sp.Integral):
            r = sp.Float(sp.Integral(fx, (X, a, b)).evalf(15), 15)
        return r

    def _derivative(self, fx, a):
        return sp.diff(fx, X).subs(X, a)

    def _sigma(self, fx, a, b):
        return sp.summation(fx, (X, sp.Integer(a), sp.Integer(b)))

    def _prod(self, fx, a, b):
        return sp.product(fx, (X, sp.Integer(a), sp.Integer(b)))

    def _parse(self, text, symbolic_xy=True):
        prepared = self._prepare(text)
        glob = {"__builtins__": {}, "Integer": sp.Integer, "Float": sp.Float, "Rational": sp.Rational,
                "Symbol": sp.Symbol, "factorial": sp.factorial, "Function": sp.Function}
        try:
            value = parse_expr(prepared, local_dict=self._locals(symbolic_xy), global_dict=glob,
                               transformations=TRANSFORMS, evaluate=True)
        except CalcError:
            raise
        except ZeroDivisionError:
            raise CalcError("Math ERROR")
        except (SyntaxError, TypeError, AttributeError, NameError, ValueError, IndexError):
            raise CalcError("Syntax ERROR")
        except Exception:
            raise CalcError("Math ERROR")
        return value

    def _run(self, fn, *args):
        future = _pool.submit(fn, *args)
        try:
            return future.result(timeout=TIME_LIMIT)
        except FutureTimeout:
            raise CalcError("Time limit: this one is too hard to work out")

    # ---- Calculate / Complex
    def evaluate(self, text, complex_mode=False):
        """Returns {"exact": MathML or None, "decimal": text, "eng": text, ...} or {"error": text}."""
        try:
            return self._run(self._evaluate, text, complex_mode)
        except CalcError as e:
            return {"error": str(e)}

    def _evaluate(self, text, complex_mode):
        store = None
        m = re.search(r"→\s*([A-FMxy])\s*$", text)
        if m:
            store, text = m.group(1), text[:m.start()]
        value = self._parse(text)
        if isinstance(value, tuple):
            return self._special(value)
        value = sp.sympify(value)
        if value.free_symbols - {X, Y}:
            raise CalcError("Syntax ERROR")
        value = value.subs({X: self.vars["x"], Y: self.vars["y"]})
        result = self._present(value, complex_mode)
        self.preans, self.ans = self.ans, result.pop("_value")
        if store:
            self.vars[store] = self.ans
            result["stored"] = store
        return result

    def _present(self, value, complex_mode=False):
        if value.has(sp.zoo, sp.nan, sp.oo, -sp.oo):
            raise CalcError("Math ERROR")
        if value.count_ops() < 60:
            try:
                value = sp.nsimplify(sp.simplify(value)) if value.has(sp.Float) and not value.is_Float else sp.simplify(value)
            except Exception:
                pass
        approx = sp.N(value, 15)
        re_part, im_part = (sp.re(approx), sp.im(approx))
        if abs(float(im_part)) > 1e-12 * max(1.0, abs(float(re_part))):
            if not complex_mode:
                raise CalcError("Math ERROR (not a real number — use Complex mode)")
            return self._present_complex(value, re_part, im_part)
        value = sp.re(value) if value.has(sp.I) else value
        out = {"_value": value, "decimal": self._num(approx.as_real_imag()[0]), "eng": format_eng(float(sp.re(approx)))}
        out["exact"] = mathml(value) if self._show_exact(value) else None
        if self.angle == "deg":
            out["dms"] = to_dms(float(sp.re(approx)))
        return out

    def _show_exact(self, v):
        if v.is_Integer or v.is_Float:
            return False
        if v.is_Rational:
            return len(str(abs(v.p))) + len(str(v.q)) <= 10
        return (not v.free_symbols and not v.has(sp.Float) and v.count_ops() <= 8
                and (v.has(sp.pi) or any(isinstance(a, sp.Pow) and a.exp.is_Rational and not a.exp.is_Integer
                                         for a in sp.preorder_traversal(v)) or v.has(sp.E)))

    def _present_complex(self, value, re_part, im_part):
        value = sp.expand(value)
        r = sp.sqrt(re_part ** 2 + im_part ** 2)
        theta = self._from_rad(sp.atan2(im_part, re_part))
        exact = mathml(value) if value.count_ops() <= 12 and not value.has(sp.Float) else None
        im_text = self._num(abs(im_part))
        decimal = f"{self._num(re_part)} {'−' if im_part < 0 else '+'} {im_text}i" if re_part != 0 \
            else f"{'−' if im_part < 0 else ''}{im_text}i"
        return {"_value": value, "exact": exact, "decimal": decimal,
                "polar": f"{self._num(r)}∠{self._num(theta)}", "eng": None}

    def _special(self, t):
        kind = t[0]
        if kind == "Pol":
            x, y = sp.sympify(t[1]), sp.sympify(t[2])
            r = sp.sqrt(x ** 2 + y ** 2)
            th = self._from_rad(sp.atan2(y, x))
            self.vars["x"], self.vars["y"] = r, th
            return {"pair": [["r", self._pair_value(r)], ["θ", self._pair_value(th)]], "note": "Stored r in x and θ in y"}
        if kind == "Rec":
            r, th = sp.sympify(t[1]), sp.sympify(t[2])
            x, y = r * sp.cos(self._to_rad(th)), r * sp.sin(self._to_rad(th))
            self.vars["x"], self.vars["y"] = x, y
            return {"pair": [["x", self._pair_value(x)], ["y", self._pair_value(y)]], "note": "Stored in x and y"}
        if kind == "Rem":
            a, b = sp.Integer(t[1]), sp.Integer(t[2])
            if b == 0:
                raise CalcError("Math ERROR")
            q, r = int(a) // int(b), int(a) % int(b)
            self.preans, self.ans = self.ans, sp.Integer(q)
            return {"pair": [["Q", {"decimal": str(q)}], ["R", {"decimal": str(r)}]]}
        if kind == "FACT":
            n = sp.sympify(t[1])
            if not n.is_Integer or n < 2 or n > 10 ** 10:
                raise CalcError("Math ERROR (needs a whole number from 2 to 10¹⁰)")
            factors = sp.factorint(int(n))
            text = "×".join(f"{p}{str(e).translate(SUPERSCRIPT) if e > 1 else ''}" for p, e in factors.items())
            return {"decimal": text, "exact": None}
        raise CalcError("Syntax ERROR")

    def _pair_value(self, v):
        v = sp.simplify(v)
        return {"decimal": self._num(sp.N(v, 15)), "exact": mathml(v) if self._show_exact(v) else None}

    # ---- memory
    def memory_add(self, text, sign=1):
        res = self.evaluate(text)
        if "error" in res:
            return res
        self.vars["M"] = self.vars["M"] + sign * self.ans
        res["memory"] = self._short(self.vars["M"])
        return res

    def clear_memory(self):
        self.__init__()
        return self.memory()

    # ---- SOLVE
    def solve(self, equation, guess=0):
        try:
            return self._run(self._solve, equation, guess)
        except CalcError as e:
            return {"error": str(e)}

    def _solve(self, equation, guess):
        if equation.count("=") > 1:
            raise CalcError("Syntax ERROR")
        lhs, rhs = (equation.split("=") + ["0"])[:2]
        f = sp.sympify(self._parse(lhs)) - sp.sympify(self._parse(rhs))
        if f.free_symbols - {X}:
            raise CalcError("Syntax ERROR (use x as the unknown)")
        if X not in f.free_symbols:
            raise CalcError("Syntax ERROR (no x in the equation)")
        guess = float(sp.N(self._parse(str(guess), symbolic_xy=False)))
        root = None
        try:
            sols = [s for s in sp.solve(f, X) if s.is_real]
            if sols:
                root = min(sols, key=lambda s: abs(float(s) - guess))
        except (NotImplementedError, TypeError, ValueError):
            pass
        if root is None:
            try:
                root = sp.nsimplify(sp.nsolve(f, X, guess), rational=False)
            except Exception:
                raise CalcError("Can't Solve")
        self.vars["x"] = root
        self.preans, self.ans = self.ans, root
        lr = sp.N(f.subs(X, root), 15)
        return {"x": self._pair_value(root), "lr": format_number(lr if abs(lr) > 1e-14 else 0, "norm1")}

    # ---- Base-N (32-bit, like the ClassWiz)
    def base_n(self, text, base=10):
        try:
            value = _BaseN(text, base).parse()
        except CalcError as e:
            return {"error": str(e)}
        unsigned = value & 0xFFFFFFFF
        signed = unsigned - (1 << 32) if unsigned & 0x80000000 else unsigned
        b = format(unsigned, "032b")
        return {"dec": str(signed), "hex": format(unsigned, "08X"), "oct": format(unsigned, "o"),
                "bin": " ".join(b[i:i + 4] for i in range(0, 32, 4)), "value": signed}

    # ---- Statistics
    def statistics(self, xs, ys=None, freqs=None, regression="lin", estimate_x=None, estimate_y=None):
        try:
            return _stats(xs, ys, freqs, regression, estimate_x, estimate_y, self._num)
        except CalcError as e:
            return {"error": str(e)}
        except (ValueError, ZeroDivisionError, OverflowError):
            return {"error": "Math ERROR"}

    # ---- Distribution
    def distribution(self, kind, params):
        try:
            return _distribution(kind, {k: float(v) for k, v in params.items() if v not in (None, "")}, self._num)
        except CalcError as e:
            return {"error": str(e)}
        except (ValueError, ZeroDivisionError, OverflowError, statistics.StatisticsError):
            return {"error": "Math ERROR"}

    # ---- Equation / inequality
    def polynomial(self, coeffs):
        try:
            return self._run(self._polynomial, coeffs)
        except CalcError as e:
            return {"error": str(e)}

    def _poly_from(self, coeffs):
        cs = [sp.sympify(self._parse(str(c), symbolic_xy=False)) for c in coeffs]
        if cs[0] == 0:
            raise CalcError("Math ERROR (the first coefficient can't be 0)")
        return cs, sum(c * X ** (len(cs) - 1 - i) for i, c in enumerate(cs))

    def _polynomial(self, coeffs):
        cs, poly = self._poly_from(coeffs)
        roots = []
        exact = sp.roots(sp.Poly(poly, X))
        if sum(exact.values()) == len(cs) - 1 and all(r.count_ops() < 40 for r in exact):
            for r, mult in exact.items():
                roots += [r] * mult
        else:
            roots = [sp.nsimplify(r) if abs(sp.im(r)) < 1e-12 else r for r in sp.Poly(poly, X).nroots(n=15)]
        roots = sorted(roots, key=lambda r: (abs(float(sp.im(sp.N(r)))) > 1e-12, float(sp.re(sp.N(r))), float(sp.im(sp.N(r)))))
        out = {"roots": [self._root_value(r) for r in roots]}
        if len(cs) == 3:  # quadratic: vertex, like the ClassWiz's extra screens
            a, b, c = cs
            vx = -b / (2 * a)
            out["vertex"] = {"x": self._pair_value(vx), "y": self._pair_value(poly.subs(X, vx)),
                             "kind": "minimum" if a > 0 else "maximum"}
        return out

    def _root_value(self, r):
        r = sp.simplify(r)
        approx = sp.N(r, 15)
        if abs(float(sp.im(approx))) > 1e-12:
            return self._present_complex(r, sp.re(approx), sp.im(approx)) | {"complex": True}
        r = sp.re(r) if r.has(sp.I) else r
        return {"decimal": self._num(sp.re(approx)), "exact": mathml(r) if self._show_exact(r) else None}

    def simultaneous(self, rows):
        try:
            return self._run(self._simultaneous, rows)
        except CalcError as e:
            return {"error": str(e)}

    def _simultaneous(self, rows):
        n = len(rows)
        syms = sp.symbols("x y z t")[:n]
        values = [[sp.sympify(self._parse(str(v), symbolic_xy=False)) for v in row] for row in rows]
        eqs = [sum(row[j] * syms[j] for j in range(n)) - row[n] for row in values]
        sol = sp.linsolve(eqs, syms)
        if not sol:
            return {"message": "No Solution"}
        sol = list(sol)[0]
        if any(s.free_symbols for s in sol):
            return {"message": "Infinite Solutions"}
        return {"solution": [[str(sym), self._pair_value(v)] for sym, v in zip(syms, sol)]}

    def inequality(self, coeffs, relation):
        try:
            return self._run(self._inequality, coeffs, relation)
        except CalcError as e:
            return {"error": str(e)}

    def _inequality(self, coeffs, relation):
        _, poly = self._poly_from(coeffs)
        rel = {">": sp.Gt, ">=": sp.Ge, "<": sp.Lt, "<=": sp.Le}[relation]
        result = sp.solve_univariate_inequality(rel(poly, 0), X, relational=False)
        if result == sp.S.Reals:
            return {"text": "All Real Numbers"}
        if result == sp.S.EmptySet:
            return {"text": "No Solution"}
        return {"text": _interval_text(result, self), "exact": None}

    # ---- Matrix / Vector
    def set_matrix(self, name, rows):
        try:
            m = sp.ImmutableMatrix([[sp.sympify(self._parse(str(v), symbolic_xy=False)) for v in row] for row in rows])
        except CalcError as e:
            return {"error": str(e)}
        (self.mats if name.startswith("Mat") else self.vcts)[name] = m if name.startswith("Mat") else m.reshape(len(m), 1)
        return {"ok": True}

    def matrix_eval(self, text, vector=False):
        try:
            return self._run(self._matrix_eval, text, vector)
        except CalcError as e:
            return {"error": str(e)}

    def _matrix_eval(self, text, vector):
        store = self.vcts if vector else self.mats
        L = self._locals()
        for k, v in store.items():
            if v is not None:
                L[k] = v
        t, f = self._to_rad, self._from_rad
        L.update({
            "det": lambda m: m.det(), "Trn": lambda m: m.T, "Identity": lambda n: sp.ImmutableMatrix(sp.eye(int(n))),
            "dot": lambda a, b: a.dot(b), "cross": lambda a, b: sp.ImmutableMatrix(a.cross(b)),
            "angle": lambda a, b: f(sp.acos(a.dot(b) / (a.norm() * b.norm()))),
            "UnitV": lambda a: a / a.norm(), "abs": lambda a: a.norm() if isinstance(a, sp.MatrixBase) else sp.Abs(a),
            "Abs": lambda a: a.norm() if isinstance(a, sp.MatrixBase) else sp.Abs(a),
        })
        prepared = self._prepare(text)
        glob = {"__builtins__": {}, "Integer": sp.Integer, "Float": sp.Float, "Rational": sp.Rational,
                "Symbol": sp.Symbol, "factorial": sp.factorial, "Function": sp.Function}
        try:
            value = parse_expr(prepared, local_dict=L, global_dict=glob, transformations=TRANSFORMS, evaluate=True)
        except (ShapeError, NonSquareMatrixError):  # before ValueError: they're subclasses of it
            raise CalcError("Dimension ERROR")
        except (SyntaxError, TypeError, AttributeError, NameError, ValueError, IndexError):
            raise CalcError("Syntax ERROR")
        except ZeroDivisionError:
            raise CalcError("Math ERROR")
        except Exception as e:
            raise CalcError("Math ERROR" if "invertible" not in str(e).lower() else "Math ERROR (not invertible)")
        if isinstance(value, sp.MatrixBase):
            value = sp.ImmutableMatrix(value.applyfunc(sp.simplify))
            store["VctAns" if vector else "MatAns"] = value
            return {"matrix": mathml(value), "rows": [[self._num(sp.N(v, 15)) for v in value.row(i)] for i in range(value.rows)]}
        value = sp.simplify(sp.sympify(value))
        if value.free_symbols:
            raise CalcError("Syntax ERROR")
        return {"decimal": self._num(sp.N(value, 15)), "exact": mathml(value) if self._show_exact(value) else None}

    # ---- Table
    def table(self, fx, gx, start, end, step):
        try:
            return self._run(self._table, fx, gx, start, end, step)
        except CalcError as e:
            return {"error": str(e)}

    def _table(self, fx, gx, start, end, step):
        start, end, step = (float(sp.N(self._parse(str(v), symbolic_xy=False))) for v in (start, end, step))
        if step <= 0 or end < start:
            raise CalcError("Range ERROR")
        count = int(math.floor((end - start) / step + 1e-9)) + 1
        if count > 200:
            raise CalcError("Range ERROR (more than 200 rows)")
        f = sp.sympify(self._parse(fx)) if fx.strip() else None
        g = sp.sympify(self._parse(gx)) if gx and gx.strip() else None
        rows = []
        for k in range(count):
            xv = sp.nsimplify(start + k * step, rational=True, tolerance=1e-12)
            row = [self._num(xv)]
            for fn in (f, g):
                if fn is None:
                    continue
                try:
                    v = sp.N(fn.subs(X, xv), 15)
                    row.append(self._num(v) if v.is_real else "ERROR")
                except Exception:
                    row.append("ERROR")
            rows.append(row)
        return {"rows": rows}

    # ---- Ratio:  a : b = x : d   or   a : b = c : x
    def ratio(self, a, b, c_or_d, which="x:d"):
        try:
            a, b, k = (sp.sympify(self._parse(str(v), symbolic_xy=False)) for v in (a, b, c_or_d))
            if (which == "x:d" and b == 0) or (which == "c:x" and a == 0):
                raise CalcError("Math ERROR")
            x = a * k / b if which == "x:d" else b * k / a
            return self._pair_value(x)
        except CalcError as e:
            return {"error": str(e)}


def _interval_text(result, calc):
    def val(v):
        v = sp.simplify(v)
        return calc._num(sp.N(v, 15)) if not calc._show_exact(v) else _plain(v)
    parts = []
    for iv in (result.args if isinstance(result, sp.Union) else [result]):
        if isinstance(iv, sp.FiniteSet):
            parts += [f"x = {val(p)}" for p in iv]
            continue
        lo, hi = iv.start, iv.end
        left = "" if lo == -sp.oo else f"{val(lo)} {'<' if iv.left_open else '≤'} "
        right = "" if hi == sp.oo else f" {'<' if iv.right_open else '≤'} {val(hi)}"
        parts.append(f"{left}x{right}")
    return ",   ".join(parts)


def _plain(v):
    return str(v).replace("sqrt", "√").replace("**", "^").replace("*", "×").replace("pi", "π")


# ---------------------------------------------------------------- Base-N parser

class _BaseN:
    """Integer expressions in BIN/OCT/DEC/HEX with and/or/xor/xnor/not/neg, 32-bit wrap."""
    BASES = {2: "01", 8: "01234567", 10: "0123456789", 16: "0123456789ABCDEF"}
    PREFIX = {"d": 10, "h": 16, "b": 2, "o": 8}

    def __init__(self, text, base):
        self.base = int(base)
        self.tokens = self._tokenize(text.replace("×", "*").replace("÷", "/").replace("−", "-"))
        self.pos = 0

    def _tokenize(self, s):
        tokens = re.findall(r"\s*(xnor|xor|and|or|not|neg|[dhbo](?=[0-9A-Fa-f])[0-9A-Fa-f]+|[0-9A-Fa-f]+|[()+\-*/])", s)
        if "".join(tokens).replace(" ", "") != re.sub(r"\s+", "", s):
            raise CalcError("Syntax ERROR")
        return tokens

    def _wrap(self, v):
        v &= 0xFFFFFFFF
        return v - (1 << 32) if v & 0x80000000 else v

    def _peek(self):
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def _take(self):
        tok = self._peek()
        self.pos += 1
        return tok

    def parse(self):
        if not self.tokens:
            raise CalcError("Syntax ERROR")
        v = self._or()
        if self._peek() is not None:
            raise CalcError("Syntax ERROR")
        return v

    def _or(self):
        v = self._and()
        while self._peek() in ("or", "xor", "xnor"):
            op, r = self._take(), self._and()
            v = self._wrap(v | r if op == "or" else v ^ r if op == "xor" else ~(v ^ r))
        return v

    def _and(self):
        v = self._add()
        while self._peek() == "and":
            self._take()
            v = self._wrap(v & self._add())
        return v

    def _add(self):
        v = self._mul()
        while self._peek() in ("+", "-"):
            op, r = self._take(), self._mul()
            v = self._wrap(v + r if op == "+" else v - r)
        return v

    def _mul(self):
        v = self._unary()
        while self._peek() in ("*", "/"):
            op, r = self._take(), self._unary()
            if op == "/":
                if r == 0:
                    raise CalcError("Math ERROR")
                v = self._wrap(int(v / r))  # truncates toward zero, like the ClassWiz
            else:
                v = self._wrap(v * r)
        return v

    def _unary(self):
        tok = self._peek()
        if tok in ("not", "neg", "-"):
            self._take()
            v = self._unary()
            return self._wrap(~v if tok == "not" else -v)
        return self._atom()

    def _atom(self):
        tok = self._take()
        if tok == "(":
            v = self._or()
            if self._take() != ")":
                raise CalcError("Syntax ERROR")
            return v
        if tok is None:
            raise CalcError("Syntax ERROR")
        base = self.base
        if tok[0] in self.PREFIX and len(tok) > 1 and not (self.base == 16 and all(c in "0123456789ABCDEFabcdef" for c in tok)):
            base, tok = self.PREFIX[tok[0]], tok[1:]
        tok = tok.upper()
        if any(c not in self.BASES[base] for c in tok):
            raise CalcError("Syntax ERROR (digit not allowed in this base)")
        v = int(tok, base)
        if v > 0xFFFFFFFF:
            raise CalcError("Math ERROR (too big for 32 bits)")
        return self._wrap(v)


# ---------------------------------------------------------------- statistics

REGRESSIONS = {
    "lin": "y = a + bx", "quad": "y = a + bx + cx²", "log": "y = a + b·ln x", "exp": "y = a·eᵇˣ",
    "abx": "y = a·bˣ", "pow": "y = a·xᵇ", "inv": "y = a + b/x",
}


def _expand(xs, freqs):
    if not freqs:
        return list(xs)
    out = []
    for x, f in zip(xs, freqs):
        if f < 0 or f != int(f):
            raise CalcError("Math ERROR (frequencies must be whole numbers ≥ 0)")
        out += [x] * int(f)
    return out


def _quartiles(data):
    s = sorted(data)
    n = len(s)
    half = n // 2
    lower, upper = s[:half], s[half + (n % 2):]
    q1 = statistics.median(lower) if lower else s[0]
    q3 = statistics.median(upper) if upper else s[-1]
    return q1, statistics.median(s), q3


def _stats(xs, ys, freqs, regression, est_x, est_y, num):
    xs = [float(v) for v in xs]
    freqs = [float(f) for f in freqs] if freqs else None
    if not xs:
        raise CalcError("No data")
    out = {}
    if ys is None:
        data = _expand(xs, freqs)
        n = len(data)
        mean = sum(data) / n
        sx2 = sum((d - mean) ** 2 for d in data)
        q1, med, q3 = _quartiles(data)
        out["results"] = [
            ["x̄", num(mean)], ["Σx", num(sum(data))], ["Σx²", num(sum(d * d for d in data))],
            ["σ²x", num(sx2 / n)], ["σx", num(math.sqrt(sx2 / n))],
            ["s²x", num(sx2 / (n - 1)) if n > 1 else "—"], ["sx", num(math.sqrt(sx2 / (n - 1))) if n > 1 else "—"],
            ["n", str(n)], ["min(x)", num(min(data))], ["Q₁", num(q1)], ["Med", num(med)],
            ["Q₃", num(q3)], ["max(x)", num(max(data))],
        ]
        return out
    ys = [float(v) for v in ys]
    if len(ys) != len(xs):
        raise CalcError("Each x needs a y")
    pts = []
    for i, (x, y) in enumerate(zip(xs, ys)):
        pts += [(x, y)] * (int(freqs[i]) if freqs else 1)
    n = len(pts)
    if n < 2:
        raise CalcError("Need at least 2 points")

    def tr(fx, fy):
        return [(fx(x), fy(y)) for x, y in pts]

    def linfit(points):
        m = len(points)
        sx = sum(p[0] for p in points); sy = sum(p[1] for p in points)
        sxx = sum(p[0] ** 2 for p in points) - sx * sx / m
        syy = sum(p[1] ** 2 for p in points) - sy * sy / m
        sxy = sum(p[0] * p[1] for p in points) - sx * sy / m
        b = sxy / sxx
        a = (sy - b * sx) / m
        r = sxy / math.sqrt(sxx * syy) if syy > 0 else 1.0
        return a, b, r

    reg = regression
    coef = {}
    if reg == "lin":
        a, b, r = linfit(pts); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a + b * x), (lambda y: (y - a) / b)
    elif reg == "quad":
        M = sp.Matrix([[sum(x ** (i + j) for x, _ in pts) for j in range(3)] for i in range(3)])
        v = sp.Matrix([sum(y * x ** i for x, y in pts) for i in range(3)])
        a, b, c = [float(t) for t in M.LUsolve(v)]
        coef, r = {"a": a, "b": b, "c": c}, None
        predict = lambda x: a + b * x + c * x * x
        def inverse(y):
            disc = b * b - 4 * c * (a - y)
            if disc < 0 or c == 0:
                return None
            return ((-b + math.sqrt(disc)) / (2 * c), (-b - math.sqrt(disc)) / (2 * c))
    elif reg == "log":
        a, b, r = linfit(tr(math.log, lambda y: y)); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a + b * math.log(x)), (lambda y: math.exp((y - a) / b))
    elif reg == "exp":
        la, b, r = linfit(tr(lambda x: x, math.log)); a = math.exp(la); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a * math.exp(b * x)), (lambda y: math.log(y / a) / b)
    elif reg == "abx":
        la, lb, r = linfit(tr(lambda x: x, math.log)); a, b = math.exp(la), math.exp(lb); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a * b ** x), (lambda y: math.log(y / a) / math.log(b))
    elif reg == "pow":
        la, b, r = linfit(tr(math.log, math.log)); a = math.exp(la); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a * x ** b), (lambda y: (y / a) ** (1 / b))
    elif reg == "inv":
        a, b, r = linfit(tr(lambda x: 1 / x, lambda y: y)); coef = {"a": a, "b": b}
        predict, inverse = (lambda x: a + b / x), (lambda y: b / (y - a))
    else:
        raise CalcError("Unknown regression")
    out["formula"] = REGRESSIONS[reg]
    out["results"] = [[k, num(v)] for k, v in coef.items()]
    if r is not None:
        out["results"].append(["r", num(r)])
    mx = sum(p[0] for p in pts) / n
    my = sum(p[1] for p in pts) / n
    out["results"] += [["x̄", num(mx)], ["ȳ", num(my)], ["n", str(n)],
                       ["Σxy", num(sum(x * y for x, y in pts))]]
    if est_x not in (None, ""):
        out["y_hat"] = num(predict(float(est_x)))
    if est_y not in (None, ""):
        xh = inverse(float(est_y))
        out["x_hat"] = "—" if xh is None else (", ".join(num(v) for v in xh) if isinstance(xh, tuple) else num(xh))
    return out


# ---------------------------------------------------------------- distributions

def _distribution(kind, p, num):
    if kind in ("normal_pd", "normal_cd", "inverse_normal"):
        sigma = p.get("sigma", 1)
        if sigma <= 0:
            raise CalcError("Math ERROR (σ must be > 0)")
        nd = statistics.NormalDist(p.get("mu", 0), sigma)
        if kind == "normal_pd":
            return {"p": num(nd.pdf(p["x"]))}
        if kind == "normal_cd":
            lo, hi = p.get("lower", -1e99), p.get("upper", 1e99)
            return {"p": num(nd.cdf(hi) - nd.cdf(lo))}
        area = p["area"]
        if not 0 < area < 1:
            raise CalcError("Math ERROR (area must be between 0 and 1)")
        return {"x": num(nd.inv_cdf(area))}
    if kind in ("binomial_pd", "binomial_cd"):
        n, prob = int(p["n"]), p["p"]
        if n < 0 or not 0 <= prob <= 1:
            raise CalcError("Math ERROR")
        pmf = lambda k: math.comb(n, k) * prob ** k * (1 - prob) ** (n - k) if 0 <= k <= n else 0.0
        if "x" not in p:  # list of all values, like the ClassWiz's List option
            vals = [pmf(k) for k in range(n + 1)]
            if kind == "binomial_cd":
                vals = [sum(vals[:k + 1]) for k in range(n + 1)]
            return {"list": [[str(k), num(v)] for k, v in enumerate(vals)]}
        k = int(p["x"])
        return {"p": num(pmf(k) if kind == "binomial_pd" else sum(pmf(i) for i in range(0, min(k, n) + 1)))}
    if kind in ("poisson_pd", "poisson_cd"):
        lam = p["lambda"]
        if lam <= 0:
            raise CalcError("Math ERROR (λ must be > 0)")
        pmf = lambda k: math.exp(-lam) * lam ** k / math.factorial(k) if k >= 0 else 0.0
        k = int(p["x"])
        return {"p": num(pmf(k) if kind == "poisson_pd" else sum(pmf(i) for i in range(k + 1)))}
    raise CalcError("Unknown distribution")
