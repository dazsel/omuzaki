## omuzaki-mcp: connected-apps token listing

- `HttpServer::tokens` lists the live access tokens (id, label, grants, issuer, paired, last use and open sessions) in id order, never their secrets; a revoked token is gone.
- Each token records when a request last presented it and passed the check (`last_used`), with no new lock.
