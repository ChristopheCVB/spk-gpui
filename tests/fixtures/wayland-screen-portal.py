"""Verify ScreenCast teardown after picker cancellation and permission failure."""

import os
import subprocess
import sys

from gi.repository import Gio, GLib


ROOT = "/org/freedesktop/portal/desktop"
XML = """
<node>
  <interface name="org.freedesktop.portal.ScreenCast">
    <property name="version" type="u" access="read"/>
    <property name="AvailableSourceTypes" type="u" access="read"/>
    <property name="AvailableCursorModes" type="u" access="read"/>
    <method name="CreateSession"><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
    <method name="SelectSources"><arg type="o" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
    <method name="Start"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.portal.Session">
    <method name="Close"/>
  </interface>
  <interface name="org.freedesktop.portal.Request">
    <signal name="Response"><arg type="u"/><arg type="a{sv}"/></signal>
  </interface>
</node>
"""

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
screen_cast, session, request = Gio.DBusNodeInfo.new_for_xml(XML).interfaces
calls = []
start_responses = iter([1, 2])


def property_value(_connection, _sender, _path, _interface, name):
    return GLib.Variant("u", {"version": 4, "AvailableSourceTypes": 2, "AvailableCursorModes": 3}[name])


def method_call(_connection, sender, _path, _interface, method, parameters, invocation):
    calls.append(method)
    if method == "Close":
        invocation.return_value(GLib.Variant("()", ()))
        return

    options = parameters.unpack()[-1]
    client = sender.removeprefix(":").replace(".", "_")
    handle = f"{ROOT}/request/{client}/{options['handle_token']}"
    response = {}
    status = 0
    if method == "CreateSession":
        session_handle = f"{ROOT}/session/{client}/{options['session_handle_token']}"
        bus.register_object(session_handle, session, method_call, None, None)
        response["session_handle"] = GLib.Variant("s", session_handle)
    elif method == "SelectSources" and (options["types"] != 2 or options["multiple"]):
        status = 2
        calls.append("unexpected selection")
    elif method == "Start":
        status = next(start_responses)

    registration = bus.register_object(handle, request, None, None, None)
    invocation.return_value(GLib.Variant("(o)", (handle,)))

    def emit_response():
        body = GLib.Variant("(ua{sv})", (status, response))
        bus.emit_signal(sender, handle, "org.freedesktop.portal.Request", "Response", body)
        bus.unregister_object(registration)
        return False

    GLib.timeout_add(25, emit_response)


bus.register_object(ROOT, screen_cast, method_call, property_value, None)
bus.call_sync(
    "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
    GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 0)),
    None, Gio.DBusCallFlags.NONE, 1000, None,
)

test = subprocess.Popen(
    sys.argv[1:], env={**os.environ, "XDG_SESSION_TYPE": "wayland", "WAYLAND_DISPLAY": "spk-fixture"}
)
loop = GLib.MainLoop()


def check_done():
    if test.poll() is None:
        return True
    loop.quit()
    return False


GLib.timeout_add(50, check_done)
loop.run()
if test.returncode:
    sys.exit(test.returncode)
if calls != ["CreateSession", "SelectSources", "Start", "Close"] * 2:
    sys.exit(f"Unexpected ScreenCast calls: {calls}")
