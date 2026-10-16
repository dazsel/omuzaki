# Webcam tracker: camera thread and preview

- Adds the webcam tracker's camera thread: a camera frame is converted to RGB, tracked, stamped on the stage clock and
  published as the newest tracker frame, with a lost frame when no face is found or the camera goes quiet or fails.
  An optional preview, at most 640 pixels wide with the face landmarks, is made only while it is on. Nothing shows it
  in the app yet.
