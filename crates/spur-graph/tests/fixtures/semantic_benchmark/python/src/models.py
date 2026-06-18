from abc import ABC
from typing import Protocol


class User:
    def __init__(self, name):
        self.name = name


class ServiceProtocol(Protocol):
    def send(self, user):
        pass


class ConcreteService(ServiceProtocol):
    def send(self, user):
        return user.name


class AbstractBase(ABC):
    def handle(self):
        pass


class ABCImpl(AbstractBase):
    def handle(self):
        return "handled"


class Meta:
    pass


class MetaOnly(metaclass=Meta):
    pass
