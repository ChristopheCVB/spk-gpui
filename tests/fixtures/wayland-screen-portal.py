"""Provide an isolated ScreenCast portal for the Linux capture tests."""

import json
import os
from pathlib import Path
import signal
import socket
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
    <method name="OpenPipeWireRemote"><arg type="o" direction="in"/><arg type="a{sv}" direction="in"/><arg type="h" direction="out"/></method>
    <method name="Start"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.portal.Session">
    <method name="Close"/>
    <signal name="Closed"><arg type="a{sv}"/></signal>
  </interface>
  <interface name="org.freedesktop.portal.Request">
    <signal name="Response"><arg type="u"/><arg type="a{sv}"/></signal>
  </interface>
</node>
"""

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
screen_cast, session, request = Gio.DBusNodeInfo.new_for_xml(XML).interfaces
calls = []
active = {}
pending = {}
closed_source = False
source = os.environ.get("SPK_TEST_WAYLAND_SOURCE")
source_type = 1 if source == "display" else 2
node = int(os.environ["SPK_TEST_PIPEWIRE_NODE"]) if source else None
rejections = iter([1, 2, "denied", None])
available_types = 3 if source else 2


def property_value(_connection, _sender, _path, _interface, name):
    return GLib.Variant("u", {"version": 4, "AvailableSourceTypes": available_types, "AvailableCursorModes": 3}[name])


def link_source():
    # The private PipeWire graph has no desktop session manager to make this link.
    objects = json.loads(subprocess.check_output(["pw-dump"], timeout=3))
    nodes = {obj["id"]: obj["info"]["props"] for obj in objects
             if obj["type"] == "PipeWire:Interface:Node"}
    ports = {obj["id"]: obj["info"]["props"] for obj in objects
             if obj["type"] == "PipeWire:Interface:Port"}
    consumer = next((id for id, props in nodes.items()
                     if props.get("media.name") == "webrtc-consume-stream"
                     and props.get("media.class") == "Stream/Input/Video"), None)
    input_port = next((id for id, props in ports.items()
                       if props.get("node.id") == consumer and props.get("port.direction") == "in"), None)
    if input_port is None:
        return True
    output_port = next(id for id, props in ports.items()
                       if props.get("node.id") == node and props.get("port.direction") == "out")
    subprocess.run(["pw-link", str(output_port), str(input_port)], check=True, timeout=3)
    return False


def method_call(_connection, sender, path, _interface, method, parameters, invocation):
    calls.append(method)
    if method == "Close":
        _, registration = active.pop(path)
        invocation.return_value(GLib.Variant("()", ()))
        bus.unregister_object(registration)
        if path in pending:
            bus.unregister_object(pending.pop(path))
        return
    if method == "OpenPipeWireRemote":
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as remote:
            remote.connect(os.path.join(os.environ["PIPEWIRE_RUNTIME_DIR"], "pipewire-0"))
            fds = Gio.UnixFDList.new()
            index = fds.append(remote.fileno())
            invocation.return_value_with_unix_fd_list(GLib.Variant("(h)", (index,)), fds)
        GLib.timeout_add(50, link_source)
        return

    options = parameters.unpack()[-1]
    client = sender.removeprefix(":").replace(".", "_")
    handle = f"{ROOT}/request/{client}/{options['handle_token']}"
    response = {}
    status = 0
    if method == "CreateSession":
        session_handle = f"{ROOT}/session/{client}/{options['session_handle_token']}"
        registration = bus.register_object(session_handle, session, method_call, None, None)
        active[session_handle] = (sender, registration)
        response["session_handle"] = GLib.Variant("s", session_handle)
    elif method == "SelectSources" and (options["types"] != available_types or options["multiple"]):
        status = 2
        calls.append("unexpected selection")
    elif method == "Start":
        status = 0 if source else next(rejections)
        if status == "denied":
            invocation.return_dbus_error("org.freedesktop.portal.Error.NotAllowed", "Permission denied")
            return
        if status == 0:
            response["streams"] = GLib.Variant(
                "a(ua{sv})", [(node, {"size": GLib.Variant("(ii)", (640, 480)),
                                     "source_type": GLib.Variant("u", source_type)})]
            )

    registration = bus.register_object(handle, request, None, None, None)
    invocation.return_value(GLib.Variant("(o)", (handle,)))

    def emit_response():
        body = GLib.Variant("(ua{sv})", (status, response))
        bus.emit_signal(sender, handle, "org.freedesktop.portal.Request", "Response", body)
        bus.unregister_object(registration)
        return False

    if status is None:
        pending[parameters.unpack()[0]] = registration
    else:
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
    global closed_source
    if source and not closed_source and Path(os.environ["SPK_TEST_WAYLAND_SCREEN_STOP"]).exists():
        assert len(active) == 1
        handle, (sender, registration) = active.popitem()
        os.kill(int(os.environ["SPK_TEST_PIPEWIRE_SOURCE_PID"]), signal.SIGTERM)
        bus.emit_signal(sender, handle, "org.freedesktop.portal.Session", "Closed", GLib.Variant("(a{sv})", ({},)))
        bus.unregister_object(registration)
        closed_source = True
    if test.poll() is None:
        return True
    loop.quit()
    return False


GLib.timeout_add(50, check_done)
loop.run()
if test.returncode:
    sys.exit(test.returncode)
if source:
    selected = ["CreateSession", "SelectSources", "Start", "OpenPipeWireRemote"]
    expected = selected + ["Close"] + selected if source == "window" else selected
    if not closed_source:
        sys.exit("The ScreenCast source was not closed by the fixture")
else:
    expected = ["CreateSession", "SelectSources", "Start", "Close"] * 4
if calls != expected or active or pending:
    sys.exit(f"Unexpected ScreenCast calls or active sessions: {calls}, {list(active)}")
