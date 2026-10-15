# licence.derive and io.check_export join the library's application records

- `licence.derive` reads `characters/<id>/applied/*` and joins each record's `rights` like a source row; the answer lists them as `applications`. A record that does not read is an error naming `applied/<app>` (#248).
- `io.check_export` and `licence.describe` raise `rights.item_not_cleared` (an error for `publish`, located at the record) for an applied item that is non-commercial, unknown or private test material, and the warning `rights.licence_pending` for the pending first-party licence on `publish`. `private` passes.
