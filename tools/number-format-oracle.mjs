// Opt-in Node 22 check: node tools/number-format-oracle.mjs [--tsv]
// Fixed synthetic binary64 bits only. --tsv writes verified bits<TAB>text rows
// for a separate consumer; neither mode is a runtime dependency or a gate.

const tsv = process.argv.length === 3 && process.argv[2] === "--tsv";
if (process.argv.length !== 2 && !tsv) {
    process.stderr.write("usage: node tools/number-format-oracle.mjs [--tsv]\n");
    process.exit(2);
}

const SIGN = 1n << 63n;
const HIDDEN = 1n << 52n;
const FRACTION = HIDDEN - 1n;
const INFINITY = 0x7ff0000000000000n;
const UNIT = 1n << 1075n;
const MAX_DIGITS = 17;
const MIN_DECADE = -324;
const PAST_MAX_DECADE = 309;
const pow10 = [1n];
for (let i = 1; i <= -MIN_DECADE + MAX_DIGITS - 1; i++) {
    pow10.push(pow10[i - 1] * 10n);
}

// Multiply an unsigned binary64 by 2^1075 so even the midpoints are integers.
// For the upper neighbor of max finite only, the infinity encoding is treated
// as the virtual next finite value 2^1024 (the overflow midpoint is below it).
function units(bits) {
    const exponent = bits >> 52n;
    const fraction = bits & FRACTION;
    return exponent === 0n ? fraction << 1n : (HIDDEN | fraction) << exponent;
}

function decadeOf(value) {
    // All positive finite intervals lie from 2^-1075 through <2^1024, so
    // 10^-324 <= value/UNIT < 10^309. Find floor(log10(value/UNIT)) exactly.
    let lo = MIN_DECADE;
    let hi = PAST_MAX_DECADE;
    while (hi - lo > 1) {
        const mid = Math.floor((lo + hi) / 2);
        const below = mid < 0
            ? value * pow10[-mid] < UNIT
            : value < UNIT * pow10[mid];
        if (below) hi = mid;
        else lo = mid;
    }
    return lo;
}

function nearestEven(numerator, denominator) {
    const quotient = numerator / denominator;
    const twiceRemainder = 2n * (numerator % denominator);
    return twiceRemainder > denominator
        || (twiceRemainder === denominator && (quotient & 1n) === 1n)
        ? quotient + 1n : quotient;
}

function shortest(bits) {
    const value = units(bits);
    const lower = (units(bits - 1n) + value) / 2n;
    const upper = (value + units(bits + 1n)) / 2n;
    // A midpoint rounds to this value iff its binary significand is even.
    const closed = (bits & 1n) === 0n;
    const firstDecade = decadeOf(lower);
    const lastDecade = decadeOf(upper);

    // With k digits, s*10^q has decade q+k-1. Thus only the decades touched
    // by the rounding interval can contribute candidates (at most two: the
    // upper/lower ratio is at most 3, attained at the smallest subnormal).
    // 17 digits suffice: rounding on the 17-digit grid gives relative error
    // <= 5e-17, strictly below the nearer binary midpoint's distance/value
    // (>= 2^-54, including powers of two, subnormals and max finite).
    for (let k = 1; k <= MAX_DIGITS; k++) {
        let best;
        for (let decade = firstDecade; decade <= lastDecade; decade++) {
            const q = decade - k + 1;
            // s*step/scale is the candidate in units of 2^-1075.
            const step = q < 0 ? UNIT : UNIT * pow10[q];
            const scale = q < 0 ? pow10[-q] : 1n;
            const lo = lower * scale;
            const hi = upper * scale;
            let first = closed ? (lo + step - 1n) / step : lo / step + 1n;
            let last = closed ? hi / step : (hi - 1n) / step;
            if (first < pow10[k - 1]) first = pow10[k - 1];
            if (last >= pow10[k]) last = pow10[k] - 1n;
            if (first > last) continue;

            const center = value * scale;
            const nearest = nearestEven(center, step);
            const s = nearest < first ? first : nearest > last ? last : nearest;
            const delta = s * step - center;
            const error = delta < 0n ? -delta : delta;
            // Compare exact distances even when candidates' scales differ.
            const left = best ? error * best.scale : 0n;
            const right = best ? best.error * scale : 0n;
            if (!best || left < right || (left === right && (s & 1n) === 0n)) {
                best = { s, q, error, scale };
            }
        }
        if (best) return best;
    }
    throw new Error(`no shortest decimal for ${hex(bits)}`);
}

function formatBits(bits) {
    const magnitude = bits & (SIGN - 1n);
    const sign = (bits & SIGN) === 0n ? "" : "-";
    if (magnitude > INFINITY) return "NaN";
    if (magnitude === INFINITY) return sign + "Infinity";
    if (magnitude === 0n) return "0";

    const { s, q } = shortest(magnitude);
    const digits = s.toString();
    const k = digits.length;
    const n = k + q;
    if (k <= n && n <= 21) return sign + digits + "0".repeat(n - k);
    if (0 < n && n <= 21) return sign + digits.slice(0, n) + "." + digits.slice(n);
    if (-6 < n && n <= 0) return sign + "0." + "0".repeat(-n) + digits;
    const mantissa = k === 1 ? digits : digits[0] + "." + digits.slice(1);
    const exponent = n - 1;
    return sign + mantissa + "e" + (exponent >= 0 ? "+" : "") + BigInt(exponent).toString();
}

function hex(bits) {
    return bits.toString(16).padStart(16, "0");
}

const landmarks = new Map([
    [0x0000000000000000n, "0"],
    [0x0000000000000001n, "5e-324"],
    [0x0000000000000002n, "1e-323"], // One-digit candidates in both decades.
    [0x000ffffffffffffen, "2.2250738585072004e-308"],
    [0x000fffffffffffffn, "2.225073858507201e-308"],
    [0x0010000000000000n, "2.2250738585072014e-308"],
    [0x0010000000000001n, "2.225073858507202e-308"],
    [0x7feffffffffffffen, "1.7976931348623155e+308"],
    [0x7fefffffffffffffn, "1.7976931348623157e+308"],
    [0x7ff0000000000000n, "Infinity"],
    [0x7ff0000000000001n, "NaN"], // Signaling encoding.
    [0x7ff8000000000000n, "NaN"], // Quiet encoding.
    [0x8000000000000000n, "0"],
    [0x8000000000000001n, "-5e-324"],
    [0xffefffffffffffffn, "-1.7976931348623157e+308"],
    [0xfff0000000000000n, "-Infinity"],
    [0xfff8000000000123n, "NaN"],
    // Decimal closest ties: exact values end in .25 and .75; round to .2/.8.
    [0x4310000000000001n, "1125899906842624.2"],
    [0x4310000000000003n, "1125899906842624.8"],
    [0xc310000000000003n, "-1125899906842624.8"],
    // 1e23 is an upper midpoint and 1.9e22 is a lower midpoint. Only the
    // even neighbors include them, and excluding them changes the shortest.
    [0x44b52d02c7e14af6n, "1e+23"],
    [0x44b52d02c7e14af7n, "1.0000000000000001e+23"],
    [0x449017f7df96be17n, "1.8999999999999998e+22"],
    [0x449017f7df96be18n, "1.9e+22"],
    // Both transitions between fixed and exponential notation.
    [0x3e7ad7f29abcaf48n, "1e-7"],
    [0x3eb0c6f7a0b5ed8dn, "0.000001"],
    [0x4415af1d78b58c40n, "100000000000000000000"],
    [0x444b1ae4d6e2ef50n, "1e+21"],
]);

const corpus = new Set(landmarks.keys());
// Nearest encodings of selected powers of ten and their immediate neighbors.
for (const bits of [
    0x0000000000000002n, 0x000730d67819e8d2n, 0x2b2bff2ee48e0530n, // -323,-308,-100
    0x3e7ad7f29abcaf48n, 0x3eb0c6f7a0b5ed8dn, 0x3ee4f8b588e368f1n, // -7,-6,-5
    0x3fb999999999999an, 0x3ff0000000000000n, 0x4024000000000000n, // -1,0,1
    0x430c6bf526340000n, 0x4341c37937e08000n, 0x4376345785d8a000n, // 15,16,17
    0x4415af1d78b58c40n, 0x444b1ae4d6e2ef50n, 0x4480f0cf064dd592n, // 20,21,22
    0x44b52d02c7e14af6n, 0x44ea784379d99db4n, 0x54b249ad2594c37dn, // 23,24,100
    0x7fe1ccf385ebc8a0n, // 308
]) {
    for (const offset of [-1n, 0n, 1n]) corpus.add(bits + offset);
}
// For every finite exponent field, take both edges and one mixed fraction,
// alternating the sign of the mixed case. The exponent-zero mixed case is
// explicitly nonzero, so it exercises the middle of the subnormal range.
for (let e = 0n; e < 0x7ffn; e++) {
    const base = e << 52n;
    const mixed = ((e + 1n) * 0x9e3779b97f4a7n) & FRACTION;
    corpus.add(base);
    corpus.add(base | FRACTION);
    corpus.add(((e & 1n) === 0n ? 0n : SIGN) | base | mixed);
}

const view = new DataView(new ArrayBuffer(8));
const rows = [];
for (const bits of corpus) {
    const expected = formatBits(bits);
    view.setBigUint64(0, bits);
    const actual = Number.prototype.toString.call(view.getFloat64(0));
    if (actual !== expected) {
        throw new Error(`${hex(bits)}: oracle=${JSON.stringify(expected)}, Node=${JSON.stringify(actual)}`);
    }
    const pinned = landmarks.get(bits);
    if (pinned !== undefined && expected !== pinned) {
        throw new Error(`${hex(bits)}: oracle/Node=${JSON.stringify(expected)}, landmark=${JSON.stringify(pinned)}`);
    }
    if (tsv) rows.push(`${hex(bits)}\t${expected}`);
}
// Emit nothing on stdout until all checks pass, including in TSV mode.
process.stdout.write(tsv
    ? rows.join("\n") + "\n"
    : `Number format oracle: ${corpus.size} fixed binary64 cases matched Node ${process.version}.\n`);
