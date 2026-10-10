#!/usr/bin/env python3
"""A stand-in status notifier watcher with a host, for close-without-tray-linux.sh.

  tray-watcher-linux.py serve
      Owns org.kde.StatusNotifierWatcher on the session bus and says a host
      registered (IsStatusNotifierHostRegistered is true). Prints
      "tray-watcher: ready" once it owns the name, and
      "tray-watcher: item <bus name> <path>" for each item that registers.
      Runs until it is ended.
  tray-watcher-linux.py menu <bus name> <path>
      Prints the registered item's menu (its dbusmenu), one row a line: the
      label, "(disabled)" after one that is off, "-" for a separator.
  tray-watcher-linux.py click <bus name> <path> <label>
      Clicks the menu row with that label, as a host does.

Needs Python's GObject bindings (python3-gi on Debian and Ubuntu).
"""

import sys
import warnings

from gi.repository import Gio, GLib

# Newer PyGObject deprecates register_object with callables, the form
# Ubuntu 24.04's needs.
warnings.filterwarnings("ignore", "Gio.DBusConnection.register_object", DeprecationWarning)

WATCHER = "org.kde.StatusNotifierWatcher"
WATCHER_PATH = "/StatusNotifierWatcher"
WATCHER_XML = f"""
<node>
  <interface name="{WATCHER}">
    <method name="RegisterStatusNotifierItem">
      <arg name="service" type="s" direction="in"/>
    </method>
    <method name="RegisterStatusNotifierHost">
      <arg name="service" type="s" direction="in"/>
    </method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered">
      <arg type="s"/>
    </signal>
    <signal name="StatusNotifierHostRegistered"/>
  </interface>
</node>
"""
ITEM = "org.kde.StatusNotifierItem"
MENU = "com.canonical.dbusmenu"
PATIENCE_MS = 5000


def serve():
    connection = Gio.bus_get_sync(Gio.BusType.SESSION)
    items = []

    def on_call(connection, sender, path, interface, method, parameters, invocation):
        (service,) = parameters.unpack()
        if method == "RegisterStatusNotifierItem":
            # libayatana-appindicator registers its object path; a name
            # registers the item at the specification's path.
            path = service if service.startswith("/") else "/StatusNotifierItem"
            name = sender if service.startswith("/") else service
            items.append(f"{name}{path}")
            print(f"tray-watcher: item {name} {path}", flush=True)
            connection.emit_signal(
                None, WATCHER_PATH, WATCHER, "StatusNotifierItemRegistered",
                GLib.Variant("(s)", (f"{name}{path}",)),
            )
        invocation.return_value(None)

    def on_get(connection, sender, path, interface, name):
        if name == "IsStatusNotifierHostRegistered":
            return GLib.Variant("b", True)
        if name == "ProtocolVersion":
            return GLib.Variant("i", 0)
        return GLib.Variant("as", items)

    info = Gio.DBusNodeInfo.new_for_xml(WATCHER_XML).interfaces[0]
    connection.register_object(WATCHER_PATH, info, on_call, on_get, None)
    loop = GLib.MainLoop()

    def on_lost(*_):
        print("tray-watcher: the watcher's name is taken", file=sys.stderr, flush=True)
        loop.quit()

    Gio.bus_own_name_on_connection(
        connection, WATCHER, Gio.BusNameOwnerFlags.DO_NOT_QUEUE,
        lambda *_: print("tray-watcher: ready", flush=True), on_lost,
    )
    loop.run()
    sys.exit(1)


def call(connection, name, path, interface, method, parameters, reply):
    return connection.call_sync(
        name, path, interface, method, parameters,
        GLib.VariantType(reply) if reply else None,
        Gio.DBusCallFlags.NONE, PATIENCE_MS, None,
    ).unpack()


def menu_rows(connection, name, path):
    """The item's menu path, then its top-level rows as (id, properties)."""
    (menu,) = call(
        connection, name, path, "org.freedesktop.DBus.Properties", "Get",
        GLib.Variant("(ss)", (ITEM, "Menu")), "(v)",
    )
    _, (_, _, children) = call(
        connection, name, menu, MENU, "GetLayout",
        GLib.Variant("(iias)", (0, 1, [])), "(u(ia{sv}av))",
    )
    return menu, [(row, properties) for row, properties, _ in children]


def main():
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "serve" and len(sys.argv) == 2:
        serve()
    if command not in ("menu", "click") or len(sys.argv) != (4 if command == "menu" else 5):
        sys.exit(__doc__)
    connection = Gio.bus_get_sync(Gio.BusType.SESSION)
    menu, rows = menu_rows(connection, sys.argv[2], sys.argv[3])
    if command == "menu":
        for _, properties in rows:
            if properties.get("type") == "separator":
                print("-")
            else:
                off = " (disabled)" if properties.get("enabled") is False else ""
                print(f"{properties.get('label', '')}{off}")
        return
    wanted = [row for row, properties in rows if properties.get("label") == sys.argv[4]]
    if len(wanted) != 1:
        sys.exit(f"tray-watcher: no single row labelled {sys.argv[4]!r}")
    call(
        connection, sys.argv[2], menu, MENU, "Event",
        GLib.Variant("(isvu)", (wanted[0], "clicked", GLib.Variant("i", 0), 0)), None,
    )


if __name__ == "__main__":
    main()
