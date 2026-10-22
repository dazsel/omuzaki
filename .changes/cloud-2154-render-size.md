# Render sizes are checked against the quality's internal size

- `render.preview` and `render.turnaround` refuse a size whose internal size is above the 4096 pixel texture limit every adapter has, naming the limits, before anything is built (#2154). `high` renders at 1.5 times the output, so its largest side is 2730; `live` and `offline` still go to 4096.
