## Default Permission

Allows rsac audio capture from JavaScript: list capturable sources, query
platform capabilities, request capture consent, start/stop captures, and
subscribe to DERIVED per-chunk meter events (rsac://chunk-meta). Does NOT
grant raw-sample delivery (subscribe_raw) — that requires the explicit
allow-subscribe-raw permission (derived-data-by-default, ADR-0014 §4.2).

#### This default permission set includes the following:

- `allow-list-targets`
- `allow-capabilities`
- `allow-request-consent`
- `allow-start-capture`
- `allow-stop-capture`
- `allow-subscribe-meta`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`rsac:allow-capabilities`

</td>
<td>

Enables the capabilities command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-capabilities`

</td>
<td>

Denies the capabilities command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-list-targets`

</td>
<td>

Enables the list_targets command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-list-targets`

</td>
<td>

Denies the list_targets command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-request-consent`

</td>
<td>

Enables the request_consent command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-request-consent`

</td>
<td>

Denies the request_consent command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-start-capture`

</td>
<td>

Enables the start_capture command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-start-capture`

</td>
<td>

Denies the start_capture command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-stop-capture`

</td>
<td>

Enables the stop_capture command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-stop-capture`

</td>
<td>

Denies the stop_capture command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-subscribe-meta`

</td>
<td>

Enables the subscribe_meta command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-subscribe-meta`

</td>
<td>

Denies the subscribe_meta command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:allow-subscribe-raw`

</td>
<td>

Enables the subscribe_raw command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`rsac:deny-subscribe-raw`

</td>
<td>

Denies the subscribe_raw command without any pre-configured scope.

</td>
</tr>
</table>
