#!/usr/bin/env python3
"""Boot an ISO in QEMU with a virtual display and check that something is drawn on the screen.

  screencheck.py ISO bios|uefi OUT_PREFIX [SECONDS]

The serial-console boot tests cannot see a black screen, which is what a real PC shows when the
kernel has no working display console. This takes screenshots (QEMU `screendump`), saves the last
one as OUT_PREFIX.png and fails when almost nothing is lit. The default boot entry waits 3 s in
GRUB, so by the end the GRUB menu is long gone and what remains is the kernel's console.
"""
import os, socket, struct, subprocess, sys, tempfile, time, zlib

LIT_MIN = 800  # pixels brighter than the threshold; a few lines of text are well above this


def read_ppm(path):
    data = open(path, "rb").read()
    parts = data.split(None, 4)
    assert parts[0] == b"P6", "not a P6 PPM"
    w, h = int(parts[1]), int(parts[2])
    pix = data[len(data) - w * h * 3:]
    return w, h, pix


def lit(pix):
    n = 0
    for i in range(0, len(pix), 3):
        if max(pix[i], pix[i + 1], pix[i + 2]) > 48:
            n += 1
    return n


def write_png(path, w, h, pix):
    raw = b"".join(b"\0" + pix[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(tag, body):
        c = struct.pack(">I", len(body)) + tag + body
        return c + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
    open(path, "wb").write(png)


def main():
    iso, fw, out = sys.argv[1:4]
    seconds = int(sys.argv[4]) if len(sys.argv) > 4 else 120
    tmp = tempfile.mkdtemp()
    sock = os.path.join(tmp, "mon.sock")
    accel = ["-enable-kvm", "-cpu", "host"] if os.access("/dev/kvm", os.W_OK) else ["-cpu", "max"]
    cmd = ["qemu-system-x86_64", "-m", "2048", "-smp", "2", "-vga", "std", "-display", "none",
           "-no-reboot", "-monitor", f"unix:{sock},server,nowait", "-cdrom", iso, "-boot", "d"] + accel
    if fw == "uefi":
        ovmf = next((p for p in ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd") if os.path.exists(p)), None)
        if not ovmf:
            sys.exit("no OVMF firmware found")
        cmd += ["-M", "q35", "-bios", ovmf]
    else:
        cmd += ["-M", "pc"]
    qemu = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            if os.path.exists(sock):
                break
            time.sleep(0.1)
        m = socket.socket(socket.AF_UNIX)
        m.connect(sock)
        m.settimeout(5)
        last = None
        t0 = time.time()
        shot = 0
        while time.time() - t0 < seconds:
            time.sleep(20)
            path = f"{out}-{shot}.ppm"
            m.sendall(f"screendump {path}\n".encode())
            time.sleep(1)
            if os.path.exists(path) and os.path.getsize(path) > 100:
                w, h, pix = read_ppm(path)
                n = lit(pix)
                print(f"[{int(time.time() - t0):4d}s] {w}x{h} lit pixels: {n}", flush=True)
                last = (w, h, pix, n)
            shot += 1
        if last is None:
            sys.exit(f"{fw}: no screenshot could be taken")
        write_png(out + ".png", *last[:3])
        for i in range(shot):
            try:
                os.remove(f"{out}-{i}.ppm")
            except OSError:
                pass
        if last[3] < LIT_MIN:
            sys.exit(f"{fw}: the screen is (nearly) black after {seconds}s: {last[3]} lit pixels")
        print(f"{fw}: the screen shows output ({last[3]} lit pixels)")
    finally:
        qemu.kill()
        qemu.wait()


main()
