#!/usr/bin/env python3
"""Writes the chronyc and AT+QGPSGNMEA replies used by the GNSS tests.

The values come from a dashboard state captured on an RM520N-GL on 2026-11-04: the
same satellites, signals, elevations, azimuths and SNRs, written back as the NMEA
sentences the module sends.
Run regenerate.sh afterwards to rebuild the expected output.
"""
from functools import reduce
from pathlib import Path

HERE = Path(__file__).resolve().parent

# talker, NMEA signal ID, [(id, el, az, snr or None)]
GSV = [
    ("GP", 1, [(5, 16, 231, 21), (6, 57, 9, 19), (9, 39, 84, 20), (11, 47, 312, 22), (12, 32, 286, 24),
               (13, 25, 291, 17), (17, 41, 131, 27), (19, 71, 116, 26), (21, 50, 233, 27), (22, 15, 184, 15),
               (4, 19, 42, None), (25, 6, 318, None)]),
    ("GP", 8, [(4, 19, 42, 17), (6, 57, 9, 16), (9, 39, 84, 15), (11, 47, 312, 15), (13, 25, 291, 16),
               (21, 50, 233, 28)]),
    ("GL", 1, [(66, 55, 144, 26), (79, 22, 268, 25), (65, 47, 40, 27), (88, 45, 45, 37), (78, 10, 218, None),
               (82, 5, 302, None), (80, 10, 320, None), (87, 4, 98, None), (81, 38, 335, None),
               (67, 11, 187, None)]),
    ("GB", 1, [(41, 43, 43, 36), (36, 11, 272, 16), (32, 62, 160, 30), (30, 49, 324, 21), (27, 57, 61, 23),
               (11, 24, 308, 12), (6, 65, 217, 25), (4, 35, 123, 27), (3, 51, 200, 31), (2, 31, 238, 23),
               (1, 49, 147, 29), (8, 79, 42, None), (9, 43, 216, None), (20, 13, 200, None),
               (28, 18, 105, None), (29, 3, 302, None), (40, 9, 221, None), (45, 11, 39, None),
               (50, 58, 317, None), (59, 46, 141, None), (60, 39, 240, None), (61, 53, 201, None),
               (62, 34, 124, None)]),
    ("GB", 3, [(27, 57, 61, 29), (28, 18, 105, 19), (30, 49, 324, 16), (32, 62, 160, 25), (41, 43, 43, 39),
               (36, 11, 272, None)]),
    ("GB", 5, [(27, 57, 61, 22), (32, 62, 160, 34), (41, 43, 43, 30), (28, 18, 105, None)]),
    ("GA", 7, [(4, 20, 155, 26), (10, 56, 324, 13), (11, 37, 290, 18), (12, 57, 10, 16), (16, 49, 242, 27),
               (28, 28, 130, 27), (31, 21, 180, 19), (33, 19, 52, 18), (6, 7, 176, None), (19, 30, 97, None),
               (25, 30, 303, None), (29, 12, 49, None)]),
    ("GA", 1, [(4, 20, 155, 27), (16, 49, 242, 30), (28, 28, 130, 27), (29, 12, 49, 17), (31, 21, 180, 16),
               (11, 37, 290, None), (12, 57, 10, None), (33, 19, 52, None)]),
    ("GQ", 1, [(2, 46, 164, 30), (3, 70, 45, 30)]),
    ("GQ", 8, [(2, 46, 164, 26), (3, 70, 45, None)]),
]
# system ID (NMEA 4.10), satellites used
GSA = [(1, [6, 9, 11, 12, 17, 19, 21, 22]), (2, [66, 65, 88]), (3, [11, 12, 16, 28, 31]),
       (4, [41, 32, 27, 6]), (5, [2, 3])]


def sentence(body):
    return "+QGPSGNMEA: $%s*%02X" % (body, reduce(lambda a, c: a ^ ord(c), body, 0))


def gsv():
    out = []
    for talker, sig, sats in GSV:
        total = (len(sats) + 3) // 4
        for n in range(total):
            fields = [f"{talker}GSV", str(total), str(n + 1), "%02d" % len(sats)]
            for sid, el, az, snr in sats[n * 4:n * 4 + 4]:
                fields += ["%02d" % sid, "%02d" % el, "%03d" % az, "" if snr is None else "%02d" % snr]
            fields.append("%X" % sig)
            out.append(sentence(",".join(fields)))
    return out


def gsa():
    out = []
    for system, used in GSA:
        ids = ["%02d" % i for i in used] + [""] * (12 - len(used))
        out.append(sentence(",".join(["GNGSA", "A", "3"] + ids + ["1.0", "0.6", "0.8", str(system)])))
    return out


GGA = [sentence("GPGGA,164910.000,3114.000000,N,12128.000000,E,1,30,0.6,11.1,M,8.0,M,,")]

TRACKING = """Reference ID    : 50505300 (PPS)
Stratum         : 1
Ref time (UTC)  : Wed Nov 04 16:49:10 2026
System time     : 0.000000016 seconds fast of NTP time
Last offset     : +0.000000134 seconds
RMS offset      : 0.000000213 seconds
Frequency       : 15.329 ppm slow
Residual freq   : +0.000 ppm
Skew            : 0.004 ppm
Root delay      : 0.000000001 seconds
Root dispersion : 0.000019077 seconds
Update interval : 16.0 seconds
Leap status     : Normal
"""
SOURCES = """MS Name/IP address         Stratum Poll Reach LastRx Last sample
===============================================================================
#* PPS                           0   4   377    17  +1113ns[+1246ns] +/- 1000ns
"""
SOURCESTATS = """Name/IP Address            NP  NR  Span  Frequency  Freq Skew  Offset  Std Dev
==============================================================================
PPS                        16   9   240     -0.000      0.004     +0ns  1005ns
"""
SERVERSTATS = """NTP packets received       : 190
NTP packets dropped        : 0
Command packets received   : 93
Command packets dropped    : 0
Client log records dropped : 0
NTS-KE connections accepted: 0
NTS-KE connections dropped : 0
Authenticated NTP packets  : 0
Interleaved NTP packets    : 0
NTP timestamps held        : 0
NTP timestamp span         : 0
NTP daemon RX timestamps   : 0
NTP daemon TX timestamps   : 190
NTP kernel RX timestamps   : 190
NTP kernel TX timestamps   : 0
NTP hardware RX timestamps : 0
NTP hardware TX timestamps : 0
"""
CLIENTS = """Hostname                      NTP   Drop Int IntL Last     Cmd   Drop Int  Last
===============================================================================
192.168.255.1                  64      0   6   -    15       0      0   -     -
192.168.11.214                 12      0   8   -   3m        0      0   -     -
192.168.11.20                   3      1  10   -   2h        0      0   -     -
192.168.11.30                   0      0   -   -     -       5      0   -   40
"""
STEPS = "Nov  4 16:20:15 sdxpinn daemon.warn chronyd[1201]: System clock was stepped by 5844601.300938 seconds\n"


def main():
    files = {
        "tracking.txt": TRACKING, "sources.txt": SOURCES, "sourcestats.txt": SOURCESTATS,
        "serverstats.txt": SERVERSTATS, "clients.txt": CLIENTS, "steps.txt": STEPS,
        "gga.txt": "\n".join(GGA) + "\n", "gsa.txt": "\n".join(gsa()) + "\n", "gsv.txt": "\n".join(gsv()) + "\n",
    }
    for name, text in files.items():
        (HERE / name).write_text(text)
    print(", ".join(files))


main()
