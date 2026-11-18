"""Render the microphone icon to PNG without third-party libraries."""
import math, struct, sys, zlib

def inside(x, y):
    # Coordinates in the 64x64 SVG space.
    def rrect(x0, y0, w, h, r):
        cx = min(max(x, x0 + r), x0 + w - r)
        cy = min(max(y, y0 + r), y0 + h - r)
        return x0 <= x <= x0 + w and y0 <= y <= y0 + h and (x - cx) ** 2 + (y - cy) ** 2 <= r * r
    def seg(ax, ay, bx, by, w):
        dx, dy = bx - ax, by - ay
        t = max(0, min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
        return (x - ax - t * dx) ** 2 + (y - ay - t * dy) ** 2 <= (w / 2) ** 2
    white = (
        rrect(24, 10, 16, 28, 8)
        or (y >= 30 and abs(math.hypot(x - 32, y - 30) - 16) <= 2)
        or (y < 30 and (math.hypot(x - 16, y - 30) <= 2 or math.hypot(x - 48, y - 30) <= 2))
        or seg(32, 46, 32, 54, 4)
        or seg(24, 54, 40, 54, 4)
    )
    return white

def render(size, path):
    ss = 3  # supersampling
    rows = []
    for py in range(size):
        row = bytearray([0])
        for px in range(size):
            acc = 0
            for sy in range(ss):
                for sx in range(ss):
                    x = (px + (sx + 0.5) / ss) * 64 / size
                    y = (py + (sy + 0.5) / ss) * 64 / size
                    # Scale content to 80% so it survives maskable cropping.
                    cx, cy = 32 + (x - 32) / 0.8, 32 + (y - 32) / 0.8
                    acc += inside(cx, cy)
            a = acc / (ss * ss)
            bg = (0x4f, 0x46, 0xe5)
            row += bytes(round(bg[i] * (1 - a) + 255 * a) for i in range(3))
        rows.append(bytes(row))
    def chunk(t, d):
        c = struct.pack('>I', len(d)) + t + d
        return c + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 2, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress(b''.join(rows), 9)) + chunk(b'IEND', b'')
    open(path, 'wb').write(png)

for s in (192, 512):
    render(s, f'{sys.argv[1]}/icon-{s}.png')
