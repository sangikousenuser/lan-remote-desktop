# Local test media

`test-pattern.mp4` is a synthetic 1920×1080 / 60fps / 300-frame, H.264 Baseline / yuv420p test pattern.
It contains no captured desktop, private data, audio, or third-party footage.
It is bundled for offline VideoToolbox/renderer testing; FFmpeg is only a build-time regeneration tool.

Regenerate with FFmpeg's `testsrc2` source:

```sh
ffmpeg -hide_banner -f lavfi -i testsrc2=size=1920x1080:rate=60 -t 5 -an \
  -c:v libx264 -preset veryfast -tune zerolatency -profile:v baseline -level 4.2 \
  -pix_fmt yuv420p -crf 28 -g 60 -movflags +faststart assets/test-pattern.mp4
```

Record a new SHA-256 if regenerating. The test runner expects exactly 300 decoded frames.

`test-frame.h264` is the first H.264 frame derived from the same synthetic fixture, used only by the TLS-to-VideoToolbox integration test. It contains no captured user desktop.
