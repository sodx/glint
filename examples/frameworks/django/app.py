"""A one-file Django app for Glint's framework check. Nothing in it mentions Glint: the global
shim hooks in when Django loads.

    python3 app.py runserver 8781 --noreload
"""
import logging
import os
import sys
import tempfile

import django
from django.conf import settings
from django.db import connection
from django.http import JsonResponse
from django.urls import path

settings.configure(
    DEBUG=True,
    ALLOWED_HOSTS=["*"],
    ROOT_URLCONF=__name__,
    SECRET_KEY="example",
    DATABASES={"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": os.path.join(tempfile.gettempdir(), "glint-django-example.sqlite3")}},
    MIDDLEWARE=[],
)
log = logging.getLogger("shop")


def orders(request, customer):
    with connection.cursor() as c:
        c.execute("create table if not exists orders (id integer primary key, customer text, total real)")
        c.execute("insert into orders (customer, total) values (%s, %s)", [customer, 42.5])
        c.execute("select id, total from orders where customer = %s", [customer])
        rows = c.fetchall()
    log.warning("customer %s has %d orders", customer, len(rows))
    return JsonResponse({"orders": len(rows)})


def boom(request):
    order = {"id": 7}
    return JsonResponse({"email": order["email"]})  # KeyError


def upstream(request):
    import requests

    requests.get("http://127.0.0.1:59999/unreachable", timeout=1)
    return JsonResponse({"ok": True})


urlpatterns = [
    path("orders/<str:customer>", orders),
    path("boom", boom),
    path("upstream", upstream),
]

if __name__ == "__main__":
    django.setup()
    from django.core.management import execute_from_command_line

    execute_from_command_line(sys.argv)
