"""A synthetic user's home under both native platform names, without changing the host."""
import os


def isolated(home, inherited=None):
    environment = dict(os.environ if inherited is None else inherited)
    environment.update(HOME=str(home), USERPROFILE=str(home))
    return environment
