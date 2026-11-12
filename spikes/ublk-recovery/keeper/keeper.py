#!/usr/bin/env python3
"""P0.12 keeper: holds memfds for the ublk daemon across kill -9, handing them back over a Unix
socket with SCM_RIGHTS. Protocol (one request per connection):
  PUT <dev> <gen>\\n + fd  -> OK\\n     (replaces any previous buffer for <dev>)
  GET <dev>\\n             -> OK <gen>\\n + fd | NONE\\n
Usage: keeper.py <socket-path>   (the path must be under $LUNIX_SPIKE_DIR)"""
import os, socket, sys

path = sys.argv[1]
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
os.chmod(path, 0o600)
srv.listen(8)
held = {}  # dev -> (fd, gen)
print("keeper ready", flush=True)
while True:
    c, _ = srv.accept()
    try:
        msg, fds, _, _ = socket.recv_fds(c, 256, 1)
        parts = msg.decode().split()
        if parts and parts[0] == "PUT" and fds:
            dev, gen = int(parts[1]), int(parts[2])
            if dev in held:
                os.close(held[dev][0])
            held[dev] = (fds[0], gen)
            c.sendall(b"OK\n")
            print(f"PUT dev={dev} gen={gen}", flush=True)
        elif parts and parts[0] == "GET":
            dev = int(parts[1])
            if dev in held:
                socket.send_fds(c, [f"OK {held[dev][1]}\n".encode()], [held[dev][0]])
                print(f"GET dev={dev} gen={held[dev][1]}", flush=True)
            else:
                c.sendall(b"NONE\n")
                print(f"GET dev={dev} none", flush=True)
        else:
            c.sendall(b"ERR\n")
    except Exception as e:
        print("error", e, flush=True)
    finally:
        c.close()
