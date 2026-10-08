# Refresh Token Max Age

A refresh token has a sliding lifetime: each refresh issues a new refresh token that lives for the
full `refresh_token_lifetime` again. A client that keeps refreshing therefore stays logged in
forever, as long as the user is enabled and the client refreshes in time.

The `refresh_max_age` of a client limits this. It is the maximum age in seconds of a login,
measured from the `auth_time` of that login, which Rauthy carries along on every refresh token:

- A refresh after `auth_time + refresh_max_age` is rejected with `invalid_grant`, and the refresh
  token is deleted. The user has to log in again.
- No token issued for the client expires later than `auth_time + refresh_max_age`. Access and ID
  token lifetimes are cut short where necessary, and `expires_in` reports the actual lifetime.
- No refresh token is issued if the access token already lasts until that time.

Without a `refresh_max_age`, which is the default, nothing changes.

It is set per client in the Admin UI under the client config, or via the clients API. Valid
values are 60 seconds up to 1 year.

```json
{
  "refresh_max_age": 72000
}
```

This is useful for command line tools and other public clients that keep a refresh token on a
device: with `refresh_max_age = 72000`, a stolen refresh token stops working at the latest 20 hours
after the user logged in, no matter how often it was refreshed. Other identity providers call the
same idea an absolute refresh token lifetime or a maximum client session.

```admonish note
For the authorization code flow, `auth_time` is the user's last login, which may be older than the
current request if the user still has a valid Rauthy session. If it is older than the
`refresh_max_age`, the token request fails with `invalid_grant`. A client can avoid this by sending
`max_age` (in seconds, at most the `refresh_max_age`) or `prompt=login` with the authorization
request, which forces a new login if necessary.

For the device authorization grant, `auth_time` is the time the tokens were issued after the
user confirmed the device.
```

```admonish caution
Refresh tokens from older Rauthy versions may not carry an `auth_time`. For a client with a
`refresh_max_age`, they are rejected, which only forces a new login.
```
