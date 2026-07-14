import socket

# No test call passes timeout= explicitly; urllib3 (and thus requests) falls
# back to the global socket default, so this one knob bounds every HTTP call
# in the suite instead of hanging a CI runner until the job timeout.
socket.setdefaulttimeout(30)
