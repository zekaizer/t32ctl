#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Print RCL request vectors produced by pyrcl's own serializers.

The request goldens in crates/t32rcl/src/wire.rs come from this script. It imports an installed
pyrcl (pip install lauterbach-trace32-rcl==1.1.6) or the directory given as the first argument.
No connection is made: a stub link records what pyrcl would transmit.
"""
import sys

if len(sys.argv) > 1:
    sys.path.insert(0, sys.argv[1])

from lauterbach.trace32.rcl._rc._address import Address  # noqa: E402
from lauterbach.trace32.rcl._rc._library import Library  # noqa: E402
from lauterbach.trace32.rcl._rc._practice import PracticeService  # noqa: E402


class StubLink:
    def __init__(self, first_id):
        self.counter = first_id - 1
        self.sent = []

    def increment_message_id(self):
        self.counter += 1
        return self.counter % 255

    def transmit(self, data):
        self.sent.append(bytes(data))

    def receive(self):
        return bytes([0, self.counter % 255]) + bytes(16)

    def getHeadersize(self):
        return 10


class Conn:
    def __init__(self, library):
        self.library = library

    def t32_exp(self, cmd, data):
        return self.library.t32_exp(cmd, data)


def library(first_id):
    lib = Library.__new__(Library)
    lib._link = StubLink(first_id)
    lib._maxpacketsize = 1024
    return lib


def address(access, value):
    a = Address.__new__(Address)
    a.access, a.value = access, value
    return a


def emit(name, first_id, call):
    lib = library(first_id)
    try:
        call(lib)
    except Exception:  # the stub reply does not satisfy every decoder; the request is recorded
        pass
    for msg in lib._link.sent:
        print(f"{name:<14} {msg.hex(' ')}")


emit("attach", 1, lambda l: l.t32_attach(1))
emit("cmd", 7, lambda l: l.t32_executecommand("PRINT 1", 4096))
emit("fnc", 1, lambda l: l.t32_executefunction("VERSION.BUILD()"))
emit("pstate", 1, lambda l: l.t32_getpracticestate())
emit("stop", 1, lambda l: l.t32_stop())
emit("getstate", 1, lambda l: l.t32_getstate())
emit("evalgetstring", 1, lambda l: l.t32_evalgetstring())
emit("memread", 7, lambda l: l.t32_readmemoryobj(address("E", 0x1000), 256))
emit("memwrite", 7, lambda l: l.t32_writememoryobj(bytes([0, 1, 2, 3, 4]), address("VM", 0x01000000)))
emit("macroget", 1, lambda l: PracticeService(Conn(l)).get_macro("hello"))
emit("macroset", 1, lambda l: PracticeService(Conn(l)).set_macro("abc", "xy"))
emit("macroget-amp", 1, lambda l: PracticeService(Conn(l)).get_macro("&abc"))
emit("longcmd", 1, lambda l: l.t32_executecommand("A" * 300, 4096))
