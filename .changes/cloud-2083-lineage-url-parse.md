# Models gate: lineage source URLs parsed by the `url` crate

- The models gate checks a lineage `url` with WHATWG parsing instead of a hand-written host check. It refuses a
  hex or overflowing dotted host such as `https://1.2.3.0x100/source`, credentials in the URL and a non-`https` scheme,
  and accepts a domain or a valid IPv4 or IPv6 host.
