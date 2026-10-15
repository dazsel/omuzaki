# licence.derive and io.check_export join the library's application records

- `licence.derive` reads `characters/<id>/applied/*` and joins each record's `rights` like a source row; the answer lists them as `applications`. A record that does not read, or has a blank licence, is an error naming `applied/<app>`; a stored record or source row with a blank licence or consent field no longer reads (#248).
- `io.check_export` and `licence.describe` raise `rights.item_not_cleared` (an error for `publish`, located at the record) for an applied item that is non-commercial, unknown or private test material. `private` passes.
