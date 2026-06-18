from helpers import inline_only, make_message, normalize
from models import ConcreteService, ServiceProtocol, User


def run(name):
    user = User(name)
    service = ConcreteService()
    service.send(user)
    make_message(user.name)
    return user


def run_hof(values):
    list(map(normalize, values))
    list(map(lambda value: inline_only(value), values))
