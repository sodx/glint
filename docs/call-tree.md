# Call tree, pauses and checks

Wrap a piece of work in `glint.measure("checkout", fn)` and everything `fn` sends nests under it:
values, SQL, logs and inner measures. The window shows the run as a call tree with a small waterfall
of where the time went. OpenTelemetry spans nest the same way.

`glint.pause(reason)` stops the program until someone presses Continue in the window, or an agent
answers through MCP, and returns the value they gave. It works in PHP, Python, Node and the browser.

Markers become tests. `glint expect`, and the MCP tool `glint_expect` for agents, waits until
events match each expectation in order and fails as soon as a program errors:

```bash
glint expect 'label:cart payload.value.total>0' 'kind:sql.query text:"insert into orders"' \
             'label:"order saved" payload.value.paid=true' -- ./scripts/checkout.sh 7
```

All terms of one expectation must hold for the same event: `label:X`, `kind:X` or `kind:sql.*`,
`text:X`, `has:PATH`, `PATH=VALUE` (also `!= > >= < <=`) and `count>=N`. It exits with 0 when
everything matched, 1 when something did not, and 2 when it could not check. Without a command it
also looks at the last 30 seconds and waits while you click through the app.

[Back to the README](../README.md)
