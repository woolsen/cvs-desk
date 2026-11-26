"""Build src/core/gbk_table.bin: the exact two-byte GBK (code page 936) table.

Python's own 'gbk' codec is the authority here, so nothing is guessed. Layout: one u16
little-endian per (lead, trail) pair, indexed as (lead - 0x81) * 191 + (trail - 0x40),
valid for lead 0x81..0xFE and trail 0x40..0xFE except 0x7F. 0 means "no character".

The output is compiled into the program with include_bytes!, so this file is how that table
was made and how to make it again. It writes next to the crate, not next to the current
folder, so it works from anywhere:

    python tools/gen_gbk.py
"""

import os
import struct
import sys

# app/tools/gen_gbk.py -> app/src/core/gbk_table.bin
DEST = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                    'src', 'core', 'gbk_table.bin')

LEADS = range(0x81, 0xFF)
TRAILS = [t for t in range(0x40, 0xFF) if t != 0x7F]

out = []
missing = 0
for lead in LEADS:
    for trail in TRAILS:
        pair = bytes((lead, trail))
        try:
            ch = pair.decode('gbk')
        except UnicodeDecodeError:
            ch = ''
        if not ch:
            out.append(0)
            missing += 1
            continue
        cp = ord(ch)
        if cp > 0xFFFF:
            # GBK has no supplementary-plane characters; refuse rather than truncate.
            print('unexpected astral code point', hex(cp), file=sys.stderr)
            sys.exit(1)
        out.append(cp)

blob = struct.pack('<%dH' % len(out), *out)
open(DEST, 'wb').write(blob)
print('wrote %d entries, %d unassigned, %d bytes to %s'
      % (len(out), missing, len(blob), DEST))
