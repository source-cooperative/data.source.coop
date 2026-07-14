import requests

# Bound every requests call in the suite so a wedged server fails a test in
# seconds instead of hanging the job until timeout-minutes kills it.
# (socket.setdefaulttimeout doesn't work here: urllib3 explicitly sets
# settimeout(None) when no timeout= is passed.) Explicit timeout= arguments
# still win over this default.
_original_request = requests.Session.request


def _bounded_request(self, *args, timeout=30, **kwargs):
    return _original_request(self, *args, timeout=timeout, **kwargs)


requests.Session.request = _bounded_request
