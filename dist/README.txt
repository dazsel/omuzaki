Omuzaki for Windows
===================

What is in this folder
----------------------
  omuzaki.exe        the program (command line, 64-bit Windows)
  README.txt         this file
  models/            the face-tracking models (see "Models" below)
  onnxruntime.dll    the ONNX Runtime library, when it is bundled (see the notices)
  notices/           licences and third-party notices for everything in this folder

How to run it
-------------
Unpack the zip anywhere you like (a folder without special characters in its path is safest), open a terminal in that
folder and run:

  omuzaki.exe --version
  omuzaki.exe --help

Keep omuzaki.exe, models/ and onnxruntime.dll side by side: the program looks for them next to itself.

Where logs go
-------------
Omuzaki writes its log to the terminal (standard error), not to a file. To keep one, redirect it:

  omuzaki.exe <command> 2> omuzaki.log

Licences
--------
Omuzaki is licensed under MIT OR Apache-2.0. notices/THIRD-PARTY-NOTICES.txt lists every library built into the program
and its licence. notices/MODELS.md says where the face models came from. notices/Apache-2.0.txt is the licence text those
models are distributed under.
