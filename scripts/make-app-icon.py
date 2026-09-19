import math, struct, zlib, sys
N = 1024
out_path = sys.argv[1]

def clamp(x, a=0.0, b=1.0): return a if x < a else b if x > b else x
def cov(d):  # signed distance (px, negative inside) -> coverage with 1px AA
    return clamp(0.5 - d)

# body: macOS icon grid, 824px rounded square
X0, Y0, S, R = 100, 100, 824, 185
def rrect_sd(x, y):
    cx, cy = X0 + S/2, Y0 + S/2
    qx, qy = abs(x - cx) - (S/2 - R), abs(y - cy) - (S/2 - R)
    ox, oy = max(qx, 0), max(qy, 0)
    return math.hypot(ox, oy) + min(max(qx, qy), 0) - R

CX, CY = 512.0, 530.0
GR, GW = 265.0, 64.0
START, SWEEP, FILL = 130.0, 280.0, 0.72
def arc_sd(x, y, sweep):
    dx, dy = x - CX, y - CY
    r = math.hypot(dx, dy)
    a = math.degrees(math.atan2(dy, dx)) % 360
    rel = (a - START) % 360
    d = float("inf")
    if rel <= sweep:
        d = abs(r - GR) - GW/2
    for ang in (START, START + sweep):
        t = math.radians(ang)
        d = min(d, math.hypot(x - (CX + GR*math.cos(t)), y - (CY + GR*math.sin(t))) - GW/2)
    return d

def over(dst, rgb, a):
    if a <= 0: return dst
    r, g, b, da = dst
    oa = a + da*(1-a)
    if oa == 0: return (0,0,0,0)
    mix = lambda s, d: (s*a + d*da*(1-a))/oa
    return (mix(rgb[0], r), mix(rgb[1], g), mix(rgb[2], b), oa)

top, bot = (0.10, 0.14, 0.21), (0.03, 0.05, 0.09)
track, fill = (1,1,1), (0.55, 0.88, 1.0)
core = (0.84, 0.96, 1.0)
rows = []
for y in range(N):
    row = bytearray([0])
    t = clamp((y - Y0) / S)
    bg = tuple(top[i]*(1-t) + bot[i]*t for i in range(3))
    for x in range(N):
        px, py = x + 0.5, y + 0.5
        c = (0.0, 0.0, 0.0, 0.0)
        body = cov(rrect_sd(px, py))
        if body > 0:
            c = over(c, bg, body)
            c = over(c, track, 0.16 * cov(arc_sd(px, py, SWEEP)) * body)
            c = over(c, fill, cov(arc_sd(px, py, SWEEP*FILL)) * body)
            d = math.hypot(px - CX, py - CY)
            glow = math.exp(-((d - 0) / 150.0) ** 2) * 0.35
            c = over(c, fill, glow * body)
            c = over(c, core, cov(d - 88) * body)
            c = over(c, fill, 0.45 * cov(abs(d - 128) - 7) * body)
        row += bytes(int(round(clamp(v)*255)) for v in (c[0], c[1], c[2], c[3]))
    rows.append(bytes(row))

def chunk(t, data):
    return struct.pack(">I", len(data)) + t + data + struct.pack(">I", zlib.crc32(t + data) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(out_path, "wb").write(png)
print("wrote", out_path)
