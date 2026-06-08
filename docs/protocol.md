# An Ember+ primer

What Ember+ is, the shape of its data, and how **emberviewer** maps onto it. No prior
experience required. (The website renders this as [`protocol.html`](protocol.html).)

## What is Ember+?

**Ember+** is an open control protocol from [Lawo](https://github.com/Lawo/ember-plus), widely
used in broadcast and pro-audio gear to expose and control device parameters over the network.
A device (or software) that publishes controllable data is a **provider**; a tool that connects
to it - like emberviewer - is a **consumer**.

Under the hood it is three nested layers over a single TCP connection (default port `9000`):

```
TCP  →  S101 frames  →  BER (TLV)  →  Glow tree
```

- **S101** is the framing layer: packet boundaries, byte escaping, a CRC-16 checksum, and keep-alive.
- **BER** (the ASN.1 Basic Encoding Rules) encodes each message as nested tag/length/value bytes.
- **Glow** is the Ember+ schema carried inside that BER: the actual tree of nodes, parameters, matrices and functions.

emberviewer implements all three from scratch in Rust (the `ember-proto` and `ember-net`
crates), so there is nothing closed in the stack.

## The provider tree

A provider exposes its data as a **tree**. Every element sits at a numeric path such as `0.1.3`
(encoded on the wire as a `RELATIVE-OID`). The element types you'll meet:

| Type | What it is |
|------|------------|
| **Node** | A branch in the tree - a container that groups other elements. |
| **Parameter** | A single value: integer, real, string, boolean or enumeration. May be read-only or read/write, with min/max, units, an enum map and more. |
| **Matrix** | A routing grid of sources × targets. Connections (crosspoints) tie a target to one or more sources. |
| **Function** | A callable operation with typed arguments and results. |
| **Stream** | A high-rate value channel, typically for audio meters and live telemetry. |

A small tree, as emberviewer shows it:

```
0  [node] EmberViewerTestProvider
  0.1  [node] parameters
    0.1.0  intParam = 42 (rw)
    0.1.1  realParam = 3.141590 (ro)
    0.1.4  enumParam = 1 (rw)
```

## How emberviewer talks to a provider

Three operations cover almost everything:

### getDirectory - browse

You never download the whole tree at once. emberviewer sends a **getDirectory** request for the
children of a node, and the provider replies with that node's immediate children. Expanding a
branch in the UI issues another `getDirectory` for it - this is the "lazy" browsing you see.

### set - control

To change a writable parameter, emberviewer sends a **set** with the new value at that
parameter's path. Booleans get dedicated set and *pulse* controls; other types are edited
inline. The provider applies the change and reports the result back.

### subscribe - watch live

Rather than polling, a consumer **subscribes** to a parameter and the provider pushes updates
whenever the value changes. emberviewer subscribes to what you're viewing so the value column
stays live, and records changes in the change log.

## Mapping at a glance

- **Address book entry** → a provider's host + port.
- **Tree row** → a Glow node, parameter, matrix or function at a numeric path.
- **Expanding a branch** → a `getDirectory` for that node's children.
- **Editing a value / toggling a crosspoint** → a `set`.
- **The live value column & change log** → active `subscribe`s.
- **"Invoke" on a function** → an Ember+ function invocation with its arguments.

## Where to go next

- Lawo's reference implementation and spec discussion: [github.com/Lawo/ember-plus](https://github.com/Lawo/ember-plus).
- Try emberviewer against the bundled test provider - see the project README.
