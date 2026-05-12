import pkg.util as util
from pkg.api import Client as APIClient

from helpers import greet
from models import User


def run(name):
    user = User(name)
    greet(user.name)
    util.log(user.name)
    client = APIClient()
    client.send(user)
    return make_message(user.name)


def make_message(value):
    return value.strip()
