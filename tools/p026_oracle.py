"""Oráculo de tests independiente del SDK para la extensión decimal publicada."""
from jsonschema import Draft202012Validator, ValidationError, validators


def maximum_decimal(_validator, maximum, value, _schema):
    if isinstance(value, str):
        # Independiente de la comparación léxica del SDK: entero Python acotado.
        if len(value) > len(maximum) or not value.isascii() or not value.isdigit() or int(value) > int(maximum):
            yield ValidationError("Decimal excede el máximo del contrato")


CanonicalValidator = validators.extend(Draft202012Validator, {"x-maximum-decimal": maximum_decimal})
