// An op with effects outside the document cannot be undone with the run.
ops::fake::write_file(#{});
